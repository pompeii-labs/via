use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mesh {
    pub id: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub slug: String,
    pub display_name: String,
    pub addresses: Vec<String>,
    pub daemon_addr: String,
    #[serde(default)]
    pub iroh_addr: Option<String>,
    pub public: bool,
    pub created_at: i64,
    pub last_seen_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    pub id: String,
    pub name: String,
    pub node_id: String,
    pub node_slug: String,
    pub node_addr: String,
    pub target: String,
    pub container: String,
    pub port: Option<String>,
    #[serde(default)]
    pub command: Vec<String>,
    pub status: ServiceStatus,
    pub published_private: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Secret {
    pub name: String,
    pub ciphertext: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    Running,
    Stopped,
}

impl ServiceStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub kind: String,
    pub payload: serde_json::Value,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshSnapshot {
    pub mesh: Option<Mesh>,
    pub nodes: Vec<Node>,
    pub services: Vec<Service>,
    pub secrets: Vec<Secret>,
    pub events: Vec<Event>,
}
