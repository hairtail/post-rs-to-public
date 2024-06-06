use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub mod codec;

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Hash, Eq)]
pub struct TaskSubmit {
    pub priority: u16,
    pub challenge: [u8; 32],
    pub start_nonce: u32,
    pub end_nonce: u32,
    pub difficulty: u64,
    pub pow_difficulty: [u8; 32],
    pub node_id: [u8; 32],
    pub pow_flags: String,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct TaskSubmitResponse {
    pub id: String,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct TaskResponse {
    pub id: String,
    pub status: u8,
    pub data: Option<HashMap<u32, u64>>,
}

impl TaskResponse {
    pub fn init(id: String) -> Self {
        Self {
            id,
            status: 1,
            data: None,
        }
    }

    pub fn completed(id: String, data: HashMap<u32, u64>) -> Self {
        Self {
            id,
            status: 2,
            data: Some(data),
        }
    }

    pub fn missed(id: String) -> Self {
        Self {
            id,
            status: 3,
            data: None,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct ServerSubmitRequest {
    pub worker_name: Option<String>,
    pub request: TaskSubmit,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub enum ServerMessage {
    ServerSubmitRequest(ServerSubmitRequest),
}
