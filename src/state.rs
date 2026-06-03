use crate::model::{Event, Mesh, MeshSnapshot, Node, Secret, Service};
use crate::paths::ViaPaths;
use crate::util::now_ts;
use anyhow::{anyhow, Context, Result};
use lux::{EmbeddedClient, EmbeddedValue, ServerConfig, ServerHandle};
use serde::{de::DeserializeOwned, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

const MESH_KEY: &str = "via:mesh";
const LOCAL_NODE_KEY: &str = "via:local_node_id";
const NODE_INDEX_KEY: &str = "via:index:nodes";
const SERVICE_INDEX_KEY: &str = "via:index:services";
const SECRET_INDEX_KEY: &str = "via:index:secrets";
const EVENT_INDEX_KEY: &str = "via:index:events";
const EVENT_TABLE: &str = "via_events";

pub struct ViaState {
    paths: ViaPaths,
    handle: Option<ServerHandle>,
    client: EmbeddedClient,
}

impl ViaState {
    pub async fn open(paths: ViaPaths) -> Result<Self> {
        paths.ensure()?;
        let handle = lux::run_with_config(ServerConfig {
            enable_resp: false,
            http_port: 0,
            data_dir: paths.lux.to_string_lossy().to_string(),
            ..ServerConfig::default()
        })
        .await
        .context("failed to start embedded Lux")?;
        let client = handle.client();
        let state = Self {
            paths,
            handle: Some(handle),
            client,
        };
        state.ensure_tables().await?;
        state.sanitize_existing_secret_events().await?;
        Ok(state)
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        self.paths.ensure()
    }

    pub fn paths(&self) -> &ViaPaths {
        &self.paths
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        self.persist().await?;
        if let Some(handle) = self.handle.take() {
            handle.shutdown_and_wait().await?;
        }
        Ok(())
    }

    pub async fn persist(&self) -> Result<()> {
        self.client.execute("SAVE", &[]).await?;
        Ok(())
    }

    pub async fn mesh(&self) -> Result<Option<Mesh>> {
        self.get_json(MESH_KEY).await
    }

    pub async fn save_mesh(&self, mesh: &Mesh) -> Result<()> {
        self.set_json(MESH_KEY, mesh).await
    }

    pub async fn save_local_node_id(&self, node_id: &str) -> Result<()> {
        self.set_json(LOCAL_NODE_KEY, &node_id.to_string()).await
    }

    pub async fn node_by_slug(&self, slug: &str) -> Result<Option<Node>> {
        self.get_json(&node_key(slug)).await
    }

    pub async fn nodes(&self) -> Result<Vec<Node>> {
        let slugs = self.index(NODE_INDEX_KEY).await?;
        let mut nodes = Vec::new();
        for slug in slugs {
            if let Some(node) = self.node_by_slug(&slug).await? {
                nodes.push(node);
            }
        }
        nodes.sort_by(|a, b| a.slug.cmp(&b.slug));
        Ok(nodes)
    }

    pub async fn local_node(&self) -> Result<Node> {
        let node_id: String = self
            .get_json(LOCAL_NODE_KEY)
            .await?
            .context("local node is not initialized; run `via init` first")?;
        self.node_by_id(&node_id)
            .await?
            .context("local node record is missing")
    }

    pub async fn node_by_id(&self, node_id: &str) -> Result<Option<Node>> {
        Ok(self
            .nodes()
            .await?
            .into_iter()
            .find(|node| node.id == node_id))
    }

    pub async fn upsert_node(&self, node: &Node) -> Result<()> {
        self.set_json(&node_key(&node.slug), node).await?;
        self.add_index(NODE_INDEX_KEY, &node.slug).await
    }

    pub async fn delete_node_slug(&self, slug: &str) -> Result<()> {
        self.client.execute("DEL", &[&node_key(slug)]).await?;
        self.remove_index(NODE_INDEX_KEY, slug).await
    }

    pub async fn service_by_name(&self, name: &str) -> Result<Option<Service>> {
        self.get_json(&service_key(name)).await
    }

    pub async fn services(&self) -> Result<Vec<Service>> {
        let names = self.index(SERVICE_INDEX_KEY).await?;
        let mut services = Vec::new();
        for name in names {
            if let Some(service) = self.service_by_name(&name).await? {
                services.push(service);
            }
        }
        services.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(services)
    }

    pub async fn upsert_service(&self, service: &Service) -> Result<()> {
        self.set_json(&service_key(&service.name), service).await?;
        self.add_index(SERVICE_INDEX_KEY, &service.name).await
    }

    pub async fn delete_service(&self, name: &str) -> Result<()> {
        self.client.execute("DEL", &[&service_key(name)]).await?;
        self.remove_index(SERVICE_INDEX_KEY, name).await
    }

    pub async fn secret_by_name(&self, name: &str) -> Result<Option<Secret>> {
        self.get_json(&secret_key(name)).await
    }

    pub async fn secrets(&self) -> Result<Vec<Secret>> {
        let names = self.index(SECRET_INDEX_KEY).await?;
        let mut secrets = Vec::new();
        for name in names {
            if let Some(secret) = self.secret_by_name(&name).await? {
                secrets.push(secret);
            }
        }
        secrets.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(secrets)
    }

    pub async fn upsert_secret(&self, secret: &Secret) -> Result<()> {
        self.set_json(&secret_key(&secret.name), secret).await?;
        self.add_index(SECRET_INDEX_KEY, &secret.name).await
    }

    pub async fn delete_secret(&self, name: &str) -> Result<()> {
        self.client.execute("DEL", &[&secret_key(name)]).await?;
        self.remove_index(SECRET_INDEX_KEY, name).await
    }

    pub async fn append_event<T: Serialize>(&self, kind: &str, payload: &T) -> Result<()> {
        let event = Event {
            id: Uuid::new_v4().to_string(),
            kind: kind.to_string(),
            payload: serde_json::to_value(payload)?,
            created_at: now_ts(),
        };
        self.upsert_event(&event).await
    }

    pub async fn events(&self, limit: usize) -> Result<Vec<Event>> {
        let ids = self.index(EVENT_INDEX_KEY).await?;
        let mut events = Vec::new();
        for id in ids {
            if let Some(event) = self.event_by_id(&id).await? {
                events.push(event);
            }
        }
        events.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        events.truncate(limit);
        Ok(events)
    }

    pub async fn event_by_id(&self, id: &str) -> Result<Option<Event>> {
        self.get_json(&event_key(id)).await
    }

    pub async fn upsert_event(&self, event: &Event) -> Result<()> {
        let event = sanitized_event(event);
        let key = event_key(&event.id);
        self.set_json(&key, &event).await?;
        self.add_index(EVENT_INDEX_KEY, &event.id).await?;
        self.insert_event_table(&event).await
    }

    pub async fn snapshot(&self) -> Result<MeshSnapshot> {
        Ok(MeshSnapshot {
            mesh: self.mesh().await?,
            nodes: self.nodes().await?,
            services: self.services().await?,
            secrets: self.secrets().await?,
            events: self.events(usize::MAX).await?,
        })
    }

    pub async fn import_snapshot(&self, snapshot: MeshSnapshot) -> Result<()> {
        let local_node_id: Option<String> = self.get_json(LOCAL_NODE_KEY).await?;
        if let Some(mesh) = snapshot.mesh {
            self.save_mesh(&mesh).await?;
        }
        for mut node in snapshot.nodes {
            if Some(node.id.as_str()) == local_node_id.as_deref() {
                if let Some(existing) = self.node_by_id(&node.id).await? {
                    node.last_seen_at = existing.last_seen_at;
                    node.daemon_addr = existing.daemon_addr;
                    node.addresses = existing.addresses;
                }
            } else {
                node.last_seen_at = None;
            }
            self.upsert_node(&node).await?;
        }
        for service in snapshot.services {
            self.upsert_service(&service).await?;
        }
        let incoming_secret_names = snapshot
            .secrets
            .iter()
            .map(|secret| secret.name.clone())
            .collect::<HashSet<_>>();
        for existing in self.secrets().await? {
            if !incoming_secret_names.contains(&existing.name) {
                self.delete_secret(&existing.name).await?;
            }
        }
        for secret in snapshot.secrets {
            self.upsert_secret(&secret).await?;
        }
        for event in snapshot.events {
            self.upsert_event(&event).await?;
        }
        Ok(())
    }

    async fn ensure_tables(&self) -> Result<()> {
        match self
            .client
            .execute(
                "TCREATE",
                &[
                    EVENT_TABLE,
                    "event_id STR UNIQUE,",
                    "kind STR,",
                    "created_at INT,",
                    "payload STR",
                ],
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if error.to_string().to_ascii_lowercase().contains("exists") => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn insert_event_table(&self, event: &Event) -> Result<()> {
        let created_at = event.created_at.to_string();
        let payload = serde_json::to_string(&event.payload)?;
        match self
            .client
            .execute(
                "TINSERT",
                &[
                    EVENT_TABLE,
                    "event_id",
                    &event.id,
                    "kind",
                    &event.kind,
                    "created_at",
                    &created_at,
                    "payload",
                    &payload,
                ],
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if error.to_string().to_ascii_lowercase().contains("unique") => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn sanitize_existing_secret_events(&self) -> Result<()> {
        for event in self.events(usize::MAX).await? {
            let sanitized = sanitized_event(&event);
            if sanitized.payload != event.payload {
                self.set_json(&event_key(&event.id), &sanitized).await?;
                self.delete_event_table_row(&event.id).await?;
                self.insert_event_table(&sanitized).await?;
            }
        }
        Ok(())
    }

    async fn delete_event_table_row(&self, event_id: &str) -> Result<()> {
        match self
            .client
            .execute(
                "TDELETE",
                &["FROM", EVENT_TABLE, "WHERE", "event_id", "=", event_id],
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if error.to_string().to_ascii_lowercase().contains("not found") => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn index(&self, key: &str) -> Result<Vec<String>> {
        Ok(self.get_json(key).await?.unwrap_or_default())
    }

    async fn add_index(&self, key: &str, value: &str) -> Result<()> {
        let mut values = self.index(key).await?;
        if !values.iter().any(|existing| existing == value) {
            values.push(value.to_string());
            values.sort();
            self.set_json(key, &values).await?;
        }
        Ok(())
    }

    async fn remove_index(&self, key: &str, value: &str) -> Result<()> {
        let mut values = self.index(key).await?;
        values.retain(|existing| existing != value);
        self.set_json(key, &values).await
    }

    async fn set_json<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let encoded = serde_json::to_string(value)?;
        self.client.execute("SET", &[key, &encoded]).await?;
        Ok(())
    }

    async fn get_json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.client.execute_value("GET", &[key]).await? {
            EmbeddedValue::Nil => Ok(None),
            EmbeddedValue::Bulk(bytes) => {
                let raw = std::str::from_utf8(&bytes).context("Lux value was not UTF-8")?;
                Ok(Some(serde_json::from_str(raw)?))
            }
            EmbeddedValue::Simple(raw) => Ok(Some(serde_json::from_str(&raw)?)),
            other => Err(anyhow!("unexpected Lux value for {key}: {other:?}")),
        }
    }
}

fn node_key(slug: &str) -> String {
    format!("via:node:{slug}")
}

fn service_key(name: &str) -> String {
    format!("via:service:{name}")
}

fn secret_key(name: &str) -> String {
    format!("via:secret:{name}")
}

fn event_key(id: &str) -> String {
    format!("via:event:{id}")
}

fn sanitized_event(event: &Event) -> Event {
    if event.kind == "secret.set" {
        if let Some(name) = event.payload.get("name").and_then(|name| name.as_str()) {
            let mut event = event.clone();
            event.payload = serde_json::json!({ "name": name });
            return event;
        }
    }
    if event.kind.starts_with("service.") {
        if let Some(mut payload) = event.payload.as_object().cloned() {
            payload.remove("command");
            let mut event = event.clone();
            event.payload = serde_json::Value::Object(payload);
            return event;
        }
    }
    if event.kind == "node.exec" {
        if let Some(mut payload) = event.payload.as_object().cloned() {
            payload.remove("command");
            let mut event = event.clone();
            event.payload = serde_json::Value::Object(payload);
            return event;
        }
    }
    event.clone()
}

#[cfg(test)]
mod tests {
    use super::{ViaState, EVENT_TABLE};
    use crate::model::{Mesh, Node};
    use crate::paths::ViaPaths;
    use tempfile::TempDir;

    #[tokio::test]
    async fn stores_nodes_in_lux() {
        let temp = TempDir::new().unwrap();
        let paths = ViaPaths {
            root: temp.path().to_path_buf(),
            lux: temp.path().join("lux"),
            logs: temp.path().join("logs"),
            bin: temp.path().join("bin"),
            mesh_key: temp.path().join("mesh.key"),
            hub_config: temp.path().join("hub.json"),
        };
        let mut state = ViaState::open(paths).await.unwrap();
        state
            .save_mesh(&Mesh {
                id: "mesh".to_string(),
                created_at: 1,
            })
            .await
            .unwrap();
        state
            .upsert_node(&Node {
                id: "node".to_string(),
                slug: "rig".to_string(),
                display_name: "rig".to_string(),
                addresses: vec!["rig".to_string()],
                daemon_addr: "rig:47819".to_string(),
                public: false,
                created_at: 1,
                last_seen_at: None,
            })
            .await
            .unwrap();

        assert_eq!(state.mesh().await.unwrap().unwrap().id, "mesh");
        assert_eq!(state.nodes().await.unwrap()[0].slug, "rig");
        state.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn imports_snapshots_into_lux_views() {
        let temp = TempDir::new().unwrap();
        let paths = ViaPaths {
            root: temp.path().to_path_buf(),
            lux: temp.path().join("lux"),
            logs: temp.path().join("logs"),
            bin: temp.path().join("bin"),
            mesh_key: temp.path().join("mesh.key"),
            hub_config: temp.path().join("hub.json"),
        };
        let mut state = ViaState::open(paths).await.unwrap();
        state
            .import_snapshot(crate::model::MeshSnapshot {
                mesh: Some(Mesh {
                    id: "mesh".to_string(),
                    created_at: 1,
                }),
                nodes: vec![Node {
                    id: "node".to_string(),
                    slug: "pi".to_string(),
                    display_name: "pi".to_string(),
                    addresses: vec!["pi.local".to_string()],
                    daemon_addr: "pi.local:47819".to_string(),
                    public: false,
                    created_at: 1,
                    last_seen_at: None,
                }],
                services: vec![],
                secrets: vec![],
                events: vec![],
            })
            .await
            .unwrap();

        assert_eq!(state.mesh().await.unwrap().unwrap().id, "mesh");
        assert_eq!(state.node_by_slug("pi").await.unwrap().unwrap().slug, "pi");
        state.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn persists_secrets() {
        let temp = TempDir::new().unwrap();
        let paths = ViaPaths {
            root: temp.path().to_path_buf(),
            lux: temp.path().join("lux"),
            logs: temp.path().join("logs"),
            bin: temp.path().join("bin"),
            mesh_key: temp.path().join("mesh.key"),
            hub_config: temp.path().join("hub.json"),
        };
        let mut state = ViaState::open(paths.clone()).await.unwrap();
        state
            .upsert_secret(&crate::model::Secret {
                name: "API_KEY".to_string(),
                ciphertext: "encrypted".to_string(),
                created_at: 1,
                updated_at: 2,
            })
            .await
            .unwrap();
        state.shutdown().await.unwrap();

        let mut state = ViaState::open(paths).await.unwrap();
        let secrets = state.secrets().await.unwrap();
        assert_eq!(secrets.len(), 1);
        assert_eq!(secrets[0].name, "API_KEY");
        state.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn imported_snapshot_reconciles_deleted_secrets() {
        let temp = TempDir::new().unwrap();
        let paths = ViaPaths {
            root: temp.path().to_path_buf(),
            lux: temp.path().join("lux"),
            logs: temp.path().join("logs"),
            bin: temp.path().join("bin"),
            mesh_key: temp.path().join("mesh.key"),
            hub_config: temp.path().join("hub.json"),
        };
        let mut state = ViaState::open(paths).await.unwrap();
        state
            .upsert_secret(&crate::model::Secret {
                name: "OLD_SECRET".to_string(),
                ciphertext: "encrypted".to_string(),
                created_at: 1,
                updated_at: 2,
            })
            .await
            .unwrap();
        state
            .import_snapshot(crate::model::MeshSnapshot {
                mesh: None,
                nodes: vec![],
                services: vec![],
                secrets: vec![],
                events: vec![],
            })
            .await
            .unwrap();

        assert!(state.secrets().await.unwrap().is_empty());
        state.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn events_are_available_in_kv_and_lux_table() {
        let temp = TempDir::new().unwrap();
        let paths = ViaPaths {
            root: temp.path().to_path_buf(),
            lux: temp.path().join("lux"),
            logs: temp.path().join("logs"),
            bin: temp.path().join("bin"),
            mesh_key: temp.path().join("mesh.key"),
            hub_config: temp.path().join("hub.json"),
        };
        let mut state = ViaState::open(paths).await.unwrap();
        state
            .append_event("audit.test", &serde_json::json!({ "safe": true }))
            .await
            .unwrap();

        let events = state.events(10).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "audit.test");

        let rows = state
            .client
            .execute("TSELECT", &["*", "FROM", EVENT_TABLE])
            .await
            .unwrap();
        let rows = String::from_utf8_lossy(&rows);
        assert!(rows.contains("audit.test"), "table rows: {rows}");
        state.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn imported_snapshot_merges_events() {
        let temp = TempDir::new().unwrap();
        let paths = ViaPaths {
            root: temp.path().to_path_buf(),
            lux: temp.path().join("lux"),
            logs: temp.path().join("logs"),
            bin: temp.path().join("bin"),
            mesh_key: temp.path().join("mesh.key"),
            hub_config: temp.path().join("hub.json"),
        };
        let mut state = ViaState::open(paths).await.unwrap();
        state
            .import_snapshot(crate::model::MeshSnapshot {
                mesh: None,
                nodes: vec![],
                services: vec![],
                secrets: vec![],
                events: vec![crate::model::Event {
                    id: "event-1".to_string(),
                    kind: "remote.event".to_string(),
                    payload: serde_json::json!({ "node": "rig" }),
                    created_at: 1,
                }],
            })
            .await
            .unwrap();

        let events = state.events(10).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "remote.event");
        state.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn secret_events_are_sanitized_before_storage() {
        let temp = TempDir::new().unwrap();
        let paths = ViaPaths {
            root: temp.path().to_path_buf(),
            lux: temp.path().join("lux"),
            logs: temp.path().join("logs"),
            bin: temp.path().join("bin"),
            mesh_key: temp.path().join("mesh.key"),
            hub_config: temp.path().join("hub.json"),
        };
        let mut state = ViaState::open(paths).await.unwrap();
        state
            .append_event(
                "secret.set",
                &serde_json::json!({
                    "name": "API_KEY",
                    "ciphertext": "encrypted",
                    "created_at": 1,
                    "updated_at": 2
                }),
            )
            .await
            .unwrap();

        let events = state.events(10).await.unwrap();
        assert_eq!(events[0].payload, serde_json::json!({ "name": "API_KEY" }));

        let rows = state
            .client
            .execute("TSELECT", &["*", "FROM", EVENT_TABLE])
            .await
            .unwrap();
        let rows = String::from_utf8_lossy(&rows);
        assert!(!rows.contains("ciphertext"), "table rows: {rows}");
        state.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn service_events_do_not_log_command_args() {
        let temp = TempDir::new().unwrap();
        let paths = ViaPaths {
            root: temp.path().to_path_buf(),
            lux: temp.path().join("lux"),
            logs: temp.path().join("logs"),
            bin: temp.path().join("bin"),
            mesh_key: temp.path().join("mesh.key"),
            hub_config: temp.path().join("hub.json"),
        };
        let mut state = ViaState::open(paths).await.unwrap();
        state
            .append_event(
                "service.started",
                &serde_json::json!({
                    "name": "api",
                    "target": "node:22",
                    "command": ["sh", "-lc", "echo secret"]
                }),
            )
            .await
            .unwrap();

        let events = state.events(10).await.unwrap();
        assert!(events[0].payload.get("command").is_none());
        let rows = state
            .client
            .execute("TSELECT", &["*", "FROM", EVENT_TABLE])
            .await
            .unwrap();
        let rows = String::from_utf8_lossy(&rows);
        assert!(!rows.contains("echo secret"), "table rows: {rows}");
        state.shutdown().await.unwrap();
    }
}
