//! The `atrium` tools across computers.
//!
//! The scheduler computer serves the `atrium` MCP server over the network on
//! its embedded node (`POST /v1/mcp?project=<root>&stream=<id>`, Streamable
//! HTTP), behind the node's usual checks: loopback or tailnet only, and a
//! Tailscale identity or a token. Agents on other computers get that URL as
//! their `atrium` server, so they can list tasks, notify, write status and
//! finish tasks without `.atrium/` being there.
//!
//! The device tools (`device_list`, `device_read`, `device_copy`) need the
//! nodes of the other computers, which only this app knows. The network
//! server runs inside this process, so it asks the Workspace for a snapshot
//! of them per call and works on it off the UI thread. A local agent's own
//! `atrium mcp` process (stdio) forwards just those tools to the same route
//! on this computer's node ([`Proxy`]).

use std::{
    path::{Path, PathBuf},
    sync::RwLock,
};

use agent_rpc::node::NodeClient;
use anyhow::{Context as _, Result, anyhow};
use atrium_core::McpServer;
use atrium_project::{
    Project,
    devices::{Entry, Nodes, Stat, same_device},
    mcp::Devices,
};
use serde_json::{Value, json};

use super::Workspace;

/// Where this computer's embedded node can be reached, once it runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Network {
    /// The node on this computer's loopback: for its own agents.
    pub local: String,
    /// The node on this computer's tailnet address: for agents on other
    /// computers. None while the node is loopback-only or off the tailnet.
    pub remote: Option<String>,
    /// The node's token, when it has one.
    pub token: Option<String>,
}

static NETWORK: RwLock<Option<Network>> = RwLock::new(None);

pub fn set_network(network: Option<Network>) {
    if let Ok(mut slot) = NETWORK.write() {
        *slot = network;
    }
}

fn network() -> Option<Network> {
    NETWORK.read().ok()?.clone()
}

/// Percent-encodes a query value.
fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The route of the `atrium` MCP server for a workstream, on `base`.
pub fn mcp_url(base: &str, root: &Path, stream: &str) -> String {
    format!(
        "{}/v1/mcp?project={}&stream={}",
        base.trim_end_matches('/'),
        encode(&root.display().to_string()),
        encode(stream)
    )
}

/// The `atrium` server for an agent on another computer: this computer's
/// node, by its tailnet address. None while there is no such address.
pub fn remote_server(root: &Path, stream: &str) -> Option<McpServer> {
    server_for(&network()?, root, stream)
}

fn server_for(network: &Network, root: &Path, stream: &str) -> Option<McpServer> {
    let base = network.remote.as_ref()?;
    let headers = match &network.token {
        Some(token) => vec![("Authorization".to_string(), format!("Bearer {token}"))],
        None => Vec::new(),
    };
    Some(McpServer::http(
        "atrium",
        &mcp_url(base, root, stream),
        headers,
    ))
}

/// Environment for a local `atrium mcp` process, telling it where this
/// computer's node is for the device tools.
pub fn local_env() -> Vec<(String, String)> {
    let Some(network) = network() else {
        return Vec::new();
    };
    let mut env = vec![("ATRIUM_NODE_URL".to_string(), network.local)];
    if let Some(token) = network.token {
        env.push(("ATRIUM_NODE_TOKEN".to_string(), token));
    }
    env
}

/// What a device tool needs of the Workspace, taken on the UI thread and
/// used on another: the nodes of the other computers and where their
/// workspace folders are.
#[derive(Clone)]
pub struct AppNodes {
    scheduler: String,
    here: PathBuf,
    devices: Vec<(String, NodeClient, Option<String>)>,
}

impl AppNodes {
    fn node(&self, device: &str) -> Result<(&NodeClient, &Option<String>)> {
        self.devices
            .iter()
            .find(|(id, ..)| same_device(id, device))
            .map(|(_, node, workspace)| (node, workspace))
            .with_context(|| format!("{device} is not connected."))
    }
}

impl Nodes for AppNodes {
    fn scheduler(&self) -> String {
        self.scheduler.clone()
    }

    fn here_workspace(&self) -> Option<PathBuf> {
        Some(self.here.clone())
    }

    fn workspace_of(&self, device: &str) -> Option<String> {
        self.node(device).ok()?.1.clone()
    }

    fn list(&self, device: &str, path: &str) -> Result<Vec<Entry>> {
        Ok(self
            .node(device)?
            .0
            .list(path)?
            .into_iter()
            .map(|e| Entry {
                name: e.name,
                dir: e.dir,
                size: e.size,
            })
            .collect())
    }

    fn stat(&self, device: &str, path: &str) -> Result<Stat> {
        let stat = self.node(device)?.0.stat(path)?;
        Ok(Stat {
            exists: stat.exists,
            dir: stat.dir,
            size: stat.size,
        })
    }

    fn read(&self, device: &str, path: &str) -> Result<Vec<u8>> {
        Ok(self.node(device)?.0.read_bytes(path)?)
    }

    fn write(&self, device: &str, path: &str, bytes: &[u8]) -> Result<()> {
        Ok(self.node(device)?.0.write_bytes(path, bytes)?)
    }

    fn mkdir(&self, device: &str, path: &str) -> Result<()> {
        Ok(self.node(device)?.0.mkdir(path)?)
    }
}

impl Workspace {
    /// A project this app runs and one of its live workstreams, as the
    /// network server may serve them: the registered root (never the
    /// caller's spelling of it), and a snapshot of the nodes.
    pub(super) fn mcp_context(
        &self,
        project: &str,
        stream: &str,
    ) -> Result<(PathBuf, AppNodes), String> {
        let wanted = PathBuf::from(project);
        let canonical = wanted.canonicalize().ok();
        let found = self.projects.iter().find(|p| {
            p.root == wanted
                || canonical
                    .as_ref()
                    .is_some_and(|c| p.root.canonicalize().ok().as_ref() == Some(c))
        });
        let Some(found) = found else {
            return Err("this Atrium does not run that project".into());
        };
        if !found
            .file
            .streams
            .iter()
            .any(|s| s.id == stream && !s.retired)
        {
            return Err(format!("the project has no workstream {stream}"));
        }
        let scheduler = if self.settings.device_name.is_empty() {
            "this computer".to_string()
        } else {
            self.settings.device_name.clone()
        };
        let devices = self
            .devices
            .iter()
            .map(|d| (d.id.clone(), d.node.clone(), self.device_workspace(&d.id)))
            .collect();
        Ok((
            found.root.clone(),
            AppNodes {
                scheduler,
                here: self.settings.atrium_workspace(),
                devices,
            },
        ))
    }
}

/// The device tools of a local agent's stdio server: forwarded to this
/// computer's own node, which holds the nodes of the other computers.
pub struct Proxy {
    client: NodeClient,
}

impl Proxy {
    /// From `ATRIUM_NODE_URL` (and `ATRIUM_NODE_TOKEN`), set by
    /// [`local_env`]; None when Atrium did not say where its node is.
    pub fn from_env() -> Option<Self> {
        let url = std::env::var("ATRIUM_NODE_URL")
            .ok()
            .filter(|u| !u.is_empty())?;
        let token = std::env::var("ATRIUM_NODE_TOKEN").ok();
        Some(Self {
            client: NodeClient::new(&url, token),
        })
    }
}

impl Devices for Proxy {
    fn call(&self, project: &Project, me: &str, name: &str, args: &Value) -> Result<String> {
        let message = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        });
        let reply = self
            .client
            .mcp(&project.root.display().to_string(), me, &message)?
            .context("Atrium did not answer")?;
        let result = &reply["result"];
        let text = result["content"][0]["text"]
            .as_str()
            .or_else(|| reply["error"]["message"].as_str())
            .unwrap_or("")
            .to_string();
        if result["isError"].as_bool().unwrap_or(false) || reply["error"].is_object() {
            return Err(anyhow!(text));
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_url_names_the_project_and_stream() {
        let url = mcp_url(
            "http://100.64.0.1:7381/",
            Path::new("/home/me/Atrium Workspace/photo"),
            "perf win",
        );
        assert_eq!(
            url,
            "http://100.64.0.1:7381/v1/mcp?project=%2Fhome%2Fme%2FAtrium%20Workspace%2Fphoto&stream=perf%20win"
        );
    }

    #[test]
    fn a_remote_agent_gets_the_url_form_with_the_token() {
        let mut net = Network {
            local: "http://127.0.0.1:7381".into(),
            remote: Some("http://100.64.0.1:7381".into()),
            token: Some("t0k".into()),
        };
        let server = server_for(&net, Path::new("/w/photo"), "perf").unwrap();
        assert_eq!(server.name, "atrium");
        assert_eq!(
            server.url.as_deref(),
            Some("http://100.64.0.1:7381/v1/mcp?project=%2Fw%2Fphoto&stream=perf")
        );
        assert_eq!(
            server.headers,
            vec![("Authorization".to_string(), "Bearer t0k".to_string())]
        );
        assert!(server.command.is_empty());
        // Off the tailnet there is nothing a remote agent could reach.
        net.remote = None;
        assert!(server_for(&net, Path::new("/w/photo"), "perf").is_none());
    }
}
