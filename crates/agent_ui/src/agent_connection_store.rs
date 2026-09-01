use std::rc::Rc;

use acp_thread::{AgentConnection, LoadError};
use agent_servers::AcpConnection;
use agent_servers::{AgentServer, AgentServerDelegate};
use anyhow::Result;
use collections::HashMap;
use futures::{FutureExt, future::Shared};
use gpui::{App, AppContext, Context, Entity, EventEmitter, SharedString, Subscription, Task};

use project::{AgentServerStore, AgentServersUpdated, Project};
use watch::Receiver;

use crate::Agent;

fn find_equivalent_entry_key<'a>(
    keys: impl Iterator<Item = &'a Agent>,
    canonical_key: &Agent,
    canonicalize: impl Fn(&Agent) -> Agent,
) -> Option<Agent> {
    let mut equivalent = None;
    for candidate in keys {
        if candidate == canonical_key {
            return Some(candidate.clone());
        }
        if canonicalize(candidate) == *canonical_key {
            equivalent = Some(candidate.clone());
        }
    }
    equivalent
}

pub enum AgentConnectionEntry {
    Connecting {
        connect_task: Shared<Task<Result<AgentConnectedState, LoadError>>>,
    },
    Connected(AgentConnectedState),
    Error {
        error: LoadError,
    },
}

#[derive(Clone)]
pub struct AgentConnectedState {
    pub connection: Rc<dyn AgentConnection>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
}

impl AgentConnectionEntry {
    pub fn wait_for_connection(&self) -> Shared<Task<Result<AgentConnectedState, LoadError>>> {
        match self {
            AgentConnectionEntry::Connecting { connect_task } => connect_task.clone(),
            AgentConnectionEntry::Connected(state) => Task::ready(Ok(state.clone())).shared(),
            AgentConnectionEntry::Error { error } => Task::ready(Err(error.clone())).shared(),
        }
    }

    pub fn status(&self) -> AgentConnectionStatus {
        match self {
            AgentConnectionEntry::Connecting { .. } => AgentConnectionStatus::Connecting,
            AgentConnectionEntry::Connected(_) => AgentConnectionStatus::Connected,
            AgentConnectionEntry::Error { .. } => AgentConnectionStatus::Disconnected,
        }
    }
}

pub enum AgentConnectionEntryEvent {
    NewVersionAvailable(SharedString),
    LoadingStatusChanged(Option<SharedString>),
}

impl EventEmitter<AgentConnectionEntryEvent> for AgentConnectionEntry {}

#[derive(Clone)]
pub struct ActiveAcpConnection {
    pub agent_id: project::AgentId,
    pub connection: Rc<AcpConnection>,
}

pub struct AgentConnectionStore {
    project: Entity<Project>,
    entries: HashMap<Agent, Entity<AgentConnectionEntry>>,
    _subscriptions: Vec<Subscription>,
}

impl AgentConnectionStore {
    pub fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        let agent_server_store = project.read(cx).agent_server_store().clone();
        let subscription = cx.subscribe(&agent_server_store, Self::handle_agent_servers_updated);
        Self {
            project,
            entries: HashMap::default(),
            _subscriptions: vec![subscription],
        }
    }

    pub fn project(&self) -> &Entity<Project> {
        &self.project
    }

    pub fn entry(&self, key: &Agent) -> Option<&Entity<AgentConnectionEntry>> {
        self.entries.get(key)
    }

    fn canonical_agent_key(&self, key: &Agent, cx: &App) -> Agent {
        match key {
            Agent::Custom { id } => {
                let agent_server_store = self.project.read(cx).agent_server_store().clone();
                let id = agent_server_store
                    .read(cx)
                    .resolve_external_agent_id(id)
                    .unwrap_or_else(|| id.clone());
                Agent::Custom { id }
            }
            _ => key.clone(),
        }
    }

    fn equivalent_entry_key(&self, key: &Agent, cx: &App) -> Option<Agent> {
        let canonical_key = self.canonical_agent_key(key, cx);
        find_equivalent_entry_key(self.entries.keys(), &canonical_key, |candidate| {
            self.canonical_agent_key(candidate, cx)
        })
    }

    pub fn connection_status(&self, key: &Agent, cx: &App) -> AgentConnectionStatus {
        self.equivalent_entry_key(key, cx)
            .and_then(|key| self.entries.get(&key))
            .map(|entry| entry.read(cx).status())
            .unwrap_or(AgentConnectionStatus::Disconnected)
    }

    pub fn agent_version(&self, key: &Agent, cx: &App) -> Option<SharedString> {
        let key = self.equivalent_entry_key(key, cx)?;
        match self.entries.get(&key)?.read(cx) {
            AgentConnectionEntry::Connected(state) => state.connection.agent_version(),
            AgentConnectionEntry::Connecting { .. } | AgentConnectionEntry::Error { .. } => None,
        }
    }

    pub fn active_acp_connections(&self, cx: &App) -> Vec<ActiveAcpConnection> {
        self.entries
            .values()
            .filter_map(|entry| match entry.read(cx) {
                AgentConnectionEntry::Connected(state) => state
                    .connection
                    .clone()
                    .downcast::<AcpConnection>()
                    .map(|connection| ActiveAcpConnection {
                        agent_id: state.connection.agent_id(),
                        connection,
                    }),
                AgentConnectionEntry::Connecting { .. } | AgentConnectionEntry::Error { .. } => {
                    None
                }
            })
            .collect()
    }

    pub fn restart_connection(
        &mut self,
        key: Agent,
        server: Rc<dyn AgentServer>,
        cx: &mut Context<Self>,
    ) -> Entity<AgentConnectionEntry> {
        let key = self.canonical_agent_key(&key, cx);
        let existing_key = self.equivalent_entry_key(&key, cx);
        if let Some(entry) = existing_key
            .as_ref()
            .and_then(|existing_key| self.entries.get(existing_key))
        {
            if matches!(entry.read(cx), AgentConnectionEntry::Connecting { .. }) {
                return entry.clone();
            }
        }

        if let Some(existing_key) = existing_key {
            self.entries.remove(&existing_key);
        }
        self.request_connection(key, server, cx)
    }

    pub fn request_fresh_connection(
        &mut self,
        key: Agent,
        server: Rc<dyn AgentServer>,
        cx: &mut Context<Self>,
    ) -> Entity<AgentConnectionEntry> {
        let key = self.canonical_agent_key(&key, cx);
        if let Some(existing_key) = self.equivalent_entry_key(&key, cx) {
            // Replace only the store's canonical cache entry. Existing thread
            // views retain their own entry (and its in-flight connect task), so
            // a superseded entry must still be allowed to finish independently.
            self.entries.remove(&existing_key);
        }
        self.request_connection(key, server, cx)
    }

    pub fn request_connection(
        &mut self,
        key: Agent,
        server: Rc<dyn AgentServer>,
        cx: &mut Context<Self>,
    ) -> Entity<AgentConnectionEntry> {
        let key = self.canonical_agent_key(&key, cx);
        if let Some(entry) = self
            .equivalent_entry_key(&key, cx)
            .and_then(|existing_key| self.entries.get(&existing_key))
        {
            return entry.clone();
        }

        let (mut new_version_rx, mut loading_status_rx, connect_task) =
            self.start_connection(server, cx);
        let connect_task = connect_task.shared();

        let entry = cx.new(|_cx| AgentConnectionEntry::Connecting {
            connect_task: connect_task.clone(),
        });

        self.entries.insert(key.clone(), entry.clone());
        cx.notify();

        cx.spawn({
            let key = key.clone();
            let entry = entry.downgrade();
            async move |this, cx| match connect_task.await {
                Ok(connected_state) => {
                    this.update(cx, move |_this, cx| {
                        entry
                            .update(cx, move |entry, cx| {
                                if let AgentConnectionEntry::Connecting { .. } = entry {
                                    *entry = AgentConnectionEntry::Connected(connected_state);
                                    cx.notify();
                                }
                            })
                            .ok();
                        cx.notify();
                    })
                    .ok();
                }
                Err(error) => {
                    this.update(cx, move |this, cx| {
                        entry
                            .update(cx, move |entry, cx| {
                                if let AgentConnectionEntry::Connecting { .. } = entry {
                                    *entry = AgentConnectionEntry::Error { error };
                                    cx.notify();
                                }
                            })
                            .ok();
                        if this.entries.get(&key) == entry.upgrade().as_ref() {
                            this.entries.remove(&key);
                        }
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();

        cx.spawn({
            let entry = entry.downgrade();
            async move |this, cx| {
                while let Ok(version) = new_version_rx.recv().await {
                    let Some(version) = version else {
                        continue;
                    };

                    this.update(cx, move |this, cx| {
                        entry
                            .update(cx, move |_entry, cx| {
                                cx.emit(AgentConnectionEntryEvent::NewVersionAvailable(
                                    version.into(),
                                ));
                            })
                            .ok();
                        if this.entries.get(&key) == entry.upgrade().as_ref() {
                            this.entries.remove(&key);
                        }
                        cx.notify();
                    })
                    .ok();
                    break;
                }
            }
        })
        .detach();

        cx.spawn({
            let entry = entry.downgrade();
            async move |this, cx| {
                while let Ok(status) = loading_status_rx.recv().await {
                    let status = status.map(SharedString::from);
                    let entry = entry.clone();
                    this.update(cx, move |_this, cx| {
                        entry
                            .update(cx, move |_entry, cx| {
                                cx.emit(AgentConnectionEntryEvent::LoadingStatusChanged(status));
                            })
                            .ok();
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();

        entry
    }

    fn handle_agent_servers_updated(
        &mut self,
        store: Entity<AgentServerStore>,
        _: &AgentServersUpdated,
        cx: &mut Context<Self>,
    ) {
        let store = store.read(cx);
        self.entries.retain(|key, _| match key {
            Agent::NativeAgent => true,
            Agent::Custom { id } => store
                .resolve_external_agent_id(id)
                .is_some_and(|canonical_id| canonical_id == *id),
            #[cfg(any(test, feature = "test-support"))]
            Agent::Stub => true,
        });
        cx.notify();
    }

    fn start_connection(
        &self,
        server: Rc<dyn AgentServer>,
        cx: &mut Context<Self>,
    ) -> (
        Receiver<Option<String>>,
        Receiver<Option<String>>,
        Task<Result<AgentConnectedState, LoadError>>,
    ) {
        let (new_version_tx, new_version_rx) = watch::channel::<Option<String>>(None);
        let (loading_status_tx, loading_status_rx) = watch::channel::<Option<String>>(None);

        let agent_server_store = self.project.read(cx).agent_server_store().clone();
        let delegate = AgentServerDelegate::new(
            agent_server_store,
            Some(new_version_tx),
            Some(loading_status_tx),
        );

        let connect_task = server.connect(delegate, self.project.clone(), cx);
        let connect_task = cx.spawn(async move |_this, _cx| match connect_task.await {
            Ok(connection) => Ok(AgentConnectedState { connection }),
            Err(err) => match err.downcast::<LoadError>() {
                Ok(load_error) => Err(load_error),
                Err(err) => Err(LoadError::Other(SharedString::from(err.to_string()))),
            },
        });
        (new_version_rx, loading_status_rx, connect_task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equivalent_cache_key_prefers_canonical_and_recognizes_legacy_aliases() {
        let legacy = Agent::Custom { id: "kimi".into() };
        let canonical = Agent::Custom {
            id: "Kimi Intrepid".into(),
        };
        let other = Agent::Custom {
            id: "GLM Intrepid".into(),
        };
        let canonicalize = |agent: &Agent| {
            if agent == &legacy {
                canonical.clone()
            } else {
                agent.clone()
            }
        };

        assert_eq!(
            find_equivalent_entry_key([&legacy, &other].into_iter(), &canonical, &canonicalize),
            Some(legacy.clone())
        );
        assert_eq!(
            find_equivalent_entry_key([&legacy, &canonical].into_iter(), &canonical, &canonicalize),
            Some(canonical)
        );
    }
}
