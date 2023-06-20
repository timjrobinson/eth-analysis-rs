mod blocks;
mod decoders;
mod heads;
mod transaction_receipts;

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use anyhow::Result;
use async_tungstenite::{
    tokio::{connect_async, TokioAdapter},
    tungstenite::Message,
    WebSocketStream,
};
use futures::stream::{FuturesOrdered, FuturesUnordered, StreamExt};
use futures::SinkExt;
use futures::{channel::oneshot, stream::SplitStream};
use lazy_static::lazy_static;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{net::TcpStream, sync::mpsc};

use crate::env;

pub use blocks::BlockHash;
pub use blocks::BlockNumber;
pub use blocks::Difficulty;
pub use blocks::ExecutionNodeBlock;
pub use blocks::TotalDifficulty;

pub use heads::stream_heads_from;
pub use heads::stream_new_heads;
pub use heads::Head;

#[cfg(test)]
pub use blocks::tests::ExecutionNodeBlockBuilder;

use self::transaction_receipts::TransactionReceipt;

lazy_static! {
    static ref EXECUTION_URL: String = env::get_env_var_unsafe("GETH_URL");
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct RpcError {
    code: i32,
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RpcMessage {
    Error { id: u16, error: RpcError },
    Result { id: u16, result: serde_json::Value },
}

impl RpcMessage {
    fn id(&self) -> u16 {
        match self {
            RpcMessage::Error { id, .. } => *id,
            RpcMessage::Result { id, .. } => *id,
        }
    }
}

struct IdPool {
    next_id: u16,
    in_use_ids: HashSet<u16>,
}

impl IdPool {
    fn new(size: usize) -> Self {
        Self {
            next_id: 0,
            in_use_ids: HashSet::with_capacity(size),
        }
    }

    fn get_next_id(&mut self) -> u16 {
        if self.in_use_ids.len() == self.in_use_ids.capacity() {
            panic!("execution node id pool exhausted")
        }

        while self.in_use_ids.contains(&self.next_id) {
            self.next_id += 1;
        }

        self.in_use_ids.insert(self.next_id);

        self.next_id
    }

    fn free_id(&mut self, id: &u16) {
        self.in_use_ids.remove(id);
    }
}

type NodeMessageRx = SplitStream<
    WebSocketStream<
        async_tungstenite::stream::Stream<
            TokioAdapter<TcpStream>,
            TokioAdapter<tokio_native_tls::TlsStream<tokio::net::TcpStream>>,
        >,
    >,
>;

type MessageHandlers = HashMap<u16, oneshot::Sender<Result<Value, RpcError>>>;

async fn handle_messages(
    mut ws_rx: NodeMessageRx,
    message_rx_map: Arc<Mutex<MessageHandlers>>,
    id_pool: Arc<Mutex<IdPool>>,
) {
    while let Some(message_result) = ws_rx.next().await {
        let message = message_result.expect("expect websocket message to be Ok");

        // We get ping messages too. Do nothing with those.
        if message.is_ping() {
            continue;
        }

        let message_bytes = message.into_data();
        let rpc_message = serde_json::from_slice::<RpcMessage>(&message_bytes)
            .expect("expect node messages to be JsonRpcMessages");

        let id = rpc_message.id();

        id_pool.lock().unwrap().free_id(&id);

        let tx = message_rx_map
            .lock()
            .unwrap()
            .remove(&id)
            .expect("expect a message handler for every received message id");

        match rpc_message {
            RpcMessage::Result { result, .. } => {
                tx.send(Ok(result)).unwrap();
            }
            RpcMessage::Error { error, .. } => {
                tx.send(Err(error)).unwrap();
            }
        };
    }
}

pub struct ExecutionNode {
    id_pool: Arc<Mutex<IdPool>>,
    message_rx_map: Arc<Mutex<MessageHandlers>>,
    message_tx: mpsc::Sender<Message>,
}

impl ExecutionNode {
    pub async fn connect() -> Self {
        let id_pool_am = Arc::new(Mutex::new(IdPool::new(u16::MAX.into())));

        let message_rx_map = Arc::new(Mutex::new(HashMap::with_capacity(u16::MAX.into())));

        let url = (*EXECUTION_URL).to_string();
        let (connected_socket, _) = connect_async(&url).await.unwrap();
        let (mut sink, stream) = connected_socket.split();

        // We'd like to read websocket messages concurrently so we read in a thread.
        // The websocket uses pipelining, so IDs are used to match request and response.
        // We'd like the request to wait for a response (from the thread).
        // Currently we use a HashMap + callback channel system, this means requests hang
        // when the websocket thread panics. Try rewriting to an implementation where the
        // sending end gets moved to the thread so that it may be dropped when the thread panics.
        // As a workaround we panic main when this thread panics.
        // Perhaps leave a tx on main, then send txs through that channel that expect a message
        // with some ID to arrive soon. This would mean the message handlers hashmap no longer has
        // to be shared and could move into the message thread.
        let default_panic = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            default_panic(info);
            std::process::exit(1);
        }));

        let id_pool_ref = id_pool_am.clone();
        let message_handlers_ref = message_rx_map.clone();
        tokio::spawn(async move {
            handle_messages(stream, message_handlers_ref, id_pool_ref).await;
        });

        let (message_tx, mut rx) = mpsc::channel(512);
        tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                sink.send(message).await.unwrap();
            }
        });

        ExecutionNode {
            id_pool: id_pool_am,
            message_rx_map,
            message_tx,
        }
    }

    pub async fn get_latest_block(&self) -> ExecutionNodeBlock {
        let value = self
            .call("eth_getBlockByNumber", &json!(("latest", false)))
            .await
            .unwrap();

        serde_json::from_value::<ExecutionNodeBlock>(value).unwrap()
    }

    pub async fn get_block_by_hash(&self, hash: &str) -> Option<ExecutionNodeBlock> {
        self.call("eth_getBlockByHash", &json!((hash, false)))
            .await
            .map_or_else(
                |err| {
                    tracing::error!("eth_getBlockByHash bad response {:?}", err);
                    None
                },
                |value| serde_json::from_value::<Option<ExecutionNodeBlock>>(value).unwrap(),
            )
    }

    pub async fn get_block_by_number(&self, number: &BlockNumber) -> Option<ExecutionNodeBlock> {
        let hex_number = format!("0x{number:x}");
        self.call("eth_getBlockByNumber", &json!((hex_number, false)))
            .await
            .map_or_else(
                |err| {
                    tracing::error!("eth_getBlockByNumber bad response {:?}", err);
                    None
                },
                |value| serde_json::from_value::<Option<ExecutionNodeBlock>>(value).unwrap(),
            )
    }

    async fn call(&self, method: &str, params: &Value) -> Result<serde_json::Value, RpcError> {
        let id = self.id_pool.lock().unwrap().get_next_id();

        let json = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        let message = serde_json::to_string(&json).unwrap();

        let (tx, rx) = oneshot::channel();

        self.message_rx_map.lock().unwrap().insert(id, tx);
        self.message_tx.send(Message::Text(message)).await.unwrap();

        rx.await.unwrap()
    }

    pub async fn get_transaction_receipt(&self, tx_hash: &str) -> Option<TransactionReceipt> {
        self.call("eth_getTransactionReceipt", &json!((tx_hash,)))
            .await
            .map(|value| serde_json::from_value::<Option<TransactionReceipt>>(value).unwrap())
            .unwrap()
    }

    pub async fn get_transaction_receipts_for_block(
        &self,
        block: &ExecutionNodeBlock,
    ) -> Option<Vec<TransactionReceipt>> {
        let mut receipt_futures = FuturesOrdered::new();

        for tx_hash in block.transactions.iter() {
            receipt_futures.push_back(self.get_transaction_receipt(tx_hash));
        }

        let mut receipts = Vec::new();

        while let Some(receipt_opt) = receipt_futures.next().await {
            match receipt_opt {
                Some(receipt) => receipts.push(receipt),
                None => return None,
            }
        }

        Some(receipts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn get_latest_block_test() {
        let node = ExecutionNode::connect().await;
        let _block = node.get_latest_block().await;
    }

    #[tokio::test]
    async fn get_block_by_number_test() {
        let node = ExecutionNode::connect().await;
        let block = node.get_block_by_number(&12965000).await;
        assert_eq!(block.unwrap().number, 12965000);
    }

    #[tokio::test]
    async fn get_unavailable_block_by_number_test() {
        let node = ExecutionNode::connect().await;
        let block = node.get_block_by_number(&999_999_999).await;
        assert_eq!(block, None);
    }

    #[tokio::test]
    async fn get_block_by_hash_test() {
        let node = ExecutionNode::connect().await;
        let block = node
            .get_block_by_hash("0x1b9595ee9ccda512b7f60beb1127095854475422ceb754a05fe537ee8163e4e7")
            .await;
        assert_eq!(block.unwrap().number, 15327142);
    }

    #[tokio::test]
    async fn get_unavailable_block_by_hash_test() {
        let node = ExecutionNode::connect().await;
        let block = node.get_block_by_hash("0xdoesnotexist").await;
        assert_eq!(block, None);
    }

    #[tokio::test]
    async fn get_transaction_receipt_test() {
        let node = ExecutionNode::connect().await;
        let tx_hash = "0xbfeb7252b08ca57a63c91ed466658109941bbca8c089e536c6ae9206b26e6108"; // Replace with a valid Ethereum transaction hash
        let receipt = node.get_transaction_receipt(tx_hash).await.unwrap();
        assert_eq!(receipt.transaction_hash, tx_hash);
    }

    #[tokio::test]
    async fn get_transaction_receipts_for_block_test() {
        let node = ExecutionNode::connect().await;
        let block_number = 17523391; // Replace with a valid Ethereum block number with some transactions
        let block = node.get_block_by_number(&block_number).await;

        assert!(block.is_some(), "Block not found");
        let block = block.unwrap();

        let receipts = node
            .get_transaction_receipts_for_block(&block)
            .await
            .expect("expect receipts");

        assert!(!receipts.is_empty(), "No transaction receipts found");

        for (i, receipt) in receipts.iter().enumerate() {
            assert_eq!(
                receipt.transaction_hash, block.transactions[i],
                "Mismatch in transaction hash"
            );
            assert_eq!(
                receipt.block_number, block_number,
                "Mismatch in block number"
            );
        }
    }
}
