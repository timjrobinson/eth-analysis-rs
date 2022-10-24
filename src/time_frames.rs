use std::{fmt::Display, slice::Iter, str::FromStr};

use chrono::Duration;
use sqlx::{
    postgres::{types::PgInterval, PgRow},
    PgExecutor, Row,
};
use thiserror::Error;

use crate::execution_chain::BlockNumber;

#[derive(Debug, PartialEq)]
pub enum LimitedTimeFrame {
    Day1,
    Day30,
    Day7,
    Hour1,
    Minute5,
}

use LimitedTimeFrame::*;

impl From<LimitedTimeFrame> for Duration {
    fn from(limited_time_frame: LimitedTimeFrame) -> Self {
        match limited_time_frame {
            Day1 => Duration::days(1),
            Day30 => Duration::days(30),
            Day7 => Duration::days(7),
            Hour1 => Duration::hours(1),
            Minute5 => Duration::minutes(5),
        }
    }
}

impl From<LimitedTimeFrame> for PgInterval {
    fn from(limited_time_frame: LimitedTimeFrame) -> Self {
        PgInterval::try_from(Into::<Duration>::into(limited_time_frame)).unwrap()
    }
}

#[derive(Debug, Error)]
pub enum ParseTimeFrameError {
    #[error("failed to parse time frame {0}")]
    UnknownTimeFrame(String),
}

impl FromStr for LimitedTimeFrame {
    type Err = ParseTimeFrameError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "m5" => Ok(Minute5),
            "h1" => Ok(Hour1),
            "d1" => Ok(Day1),
            "d7" => Ok(Day7),
            "d30" => Ok(Day30),
            unknown_time_frame => Err(ParseTimeFrameError::UnknownTimeFrame(
                unknown_time_frame.to_string(),
            )),
        }
    }
}

impl Display for LimitedTimeFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use LimitedTimeFrame::*;
        match self {
            Day1 => write!(f, "d1"),
            Day30 => write!(f, "d30"),
            Day7 => write!(f, "d7"),
            Hour1 => write!(f, "h1"),
            Minute5 => write!(f, "m5"),
        }
    }
}

impl LimitedTimeFrame {
    pub fn get_postgres_interval(&self) -> PgInterval {
        match self {
            Day1 => PgInterval {
                months: 0,
                days: 1,
                microseconds: 0,
            },
            Day30 => PgInterval {
                months: 0,
                days: 30,
                microseconds: 0,
            },
            Day7 => PgInterval {
                months: 0,
                days: 7,
                microseconds: 0,
            },
            Hour1 => PgInterval {
                months: 0,
                days: 0,
                microseconds: Duration::hours(1).num_microseconds().unwrap(),
            },
            Minute5 => PgInterval {
                months: 0,
                days: 0,
                microseconds: Duration::minutes(5).num_microseconds().unwrap(),
            },
        }
    }

    pub fn to_db_key(&self) -> &'_ str {
        match self {
            Day1 => "d1",
            Day30 => "d1",
            Day7 => "d7",
            Hour1 => "h1",
            Minute5 => "m5",
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum TimeFrame {
    #[allow(dead_code)]
    All,
    Limited(LimitedTimeFrame),
}

impl From<LimitedTimeFrame> for TimeFrame {
    fn from(limited_time_frame: LimitedTimeFrame) -> Self {
        TimeFrame::Limited(limited_time_frame)
    }
}

impl FromStr for TimeFrame {
    type Err = ParseTimeFrameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "all" => Ok(TimeFrame::All),
            unknown_time_frame => match unknown_time_frame.parse::<LimitedTimeFrame>() {
                Ok(limited_time_frame) => Ok(TimeFrame::Limited(limited_time_frame)),
                Err(err) => Err(err),
            },
        }
    }
}

impl TimeFrame {
    pub fn get_epoch_count(self) -> f64 {
        match self {
            TimeFrame::All => unimplemented!(),
            TimeFrame::Limited(limited_time_frame) => match limited_time_frame {
                Day1 => 225.0,
                Day30 => 6750.0,
                Day7 => 1575.0,
                Hour1 => 9.375,
                Minute5 => 0.78125,
            },
        }
    }

    pub fn to_db_key(&self) -> &'_ str {
        use TimeFrame::*;
        match self {
            All => "all",
            Limited(limited_time_frame) => limited_time_frame.to_db_key(),
        }
    }
}

static TIME_FRAMES: [TimeFrame; 6] = [
    TimeFrame::Limited(Minute5),
    TimeFrame::Limited(Hour1),
    TimeFrame::Limited(Day1),
    TimeFrame::Limited(Day7),
    TimeFrame::Limited(Day30),
    TimeFrame::All,
];

impl TimeFrame {
    pub fn iterator() -> Iter<'static, TimeFrame> {
        TIME_FRAMES.iter()
    }
}

pub async fn get_earliest_block_number(
    executor: impl PgExecutor<'_>,
    limited_time_frame: &LimitedTimeFrame,
) -> sqlx::Result<Option<BlockNumber>> {
    sqlx::query(
        "
            SELECT
                block_number
            FROM
                blocks_next
            AND
                timestamp >= NOW() - $1
        ",
    )
    .bind(limited_time_frame.get_postgres_interval())
    .map(|row: PgRow| row.get::<i32, _>("block_number").try_into().unwrap())
    .fetch_optional(executor)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_frame_iter_test() {
        let time_frames = TimeFrame::iterator().collect::<Vec<&TimeFrame>>();
        let expected = vec![
            &TimeFrame::Limited(Minute5),
            &TimeFrame::Limited(Hour1),
            &TimeFrame::Limited(Day1),
            &TimeFrame::Limited(Day7),
            &TimeFrame::Limited(Day30),
            &TimeFrame::All,
        ];

        assert_eq!(expected, time_frames);
    }

    #[test]
    fn parse_test() {
        let time_frame = "all".parse::<TimeFrame>().unwrap();
        assert_eq!(time_frame, TimeFrame::All);

        let limited_time_frame = "d30".parse::<TimeFrame>().unwrap();
        assert_eq!(limited_time_frame, TimeFrame::Limited(Day30))
    }

    #[test]
    fn to_db_key_test() {
        let time_frame_key = TimeFrame::All.to_db_key();
        assert_eq!(time_frame_key, "all");

        let limited_time_frame_key = TimeFrame::Limited(Day1).to_db_key();
        assert_eq!(limited_time_frame_key, "d1");
    }
}
