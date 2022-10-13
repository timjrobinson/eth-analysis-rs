use crate::env::{self, Env};

pub fn init_with_env() {
    match *env::ENV {
        Env::Dev => fmt::init(),
        Env::Prod => tracing_subscriber::registry()
            .with(Layer::default().json())
            .with(EnvFilter::from_default_env())
            .init(),
        Env::Stag => tracing_subscriber::registry()
            .with(Layer::default().json())
            .with(EnvFilter::from_default_env())
            .init(),
    }
}
