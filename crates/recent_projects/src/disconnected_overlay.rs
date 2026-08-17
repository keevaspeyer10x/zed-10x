use futures::{FutureExt as _, channel::oneshot};
use gpui::{ClickEvent, DismissEvent, EventEmitter, FocusHandle, Focusable, Render, WeakEntity};
use project::project_settings::ProjectSettings;
use remote::RemoteConnectionOptions;
use settings::Settings;
use ui::{ElevationIndex, Modal, ModalFooter, ModalHeader, Section, prelude::*};
use workspace::{
    ModalView, MultiWorkspace, OpenOptions, Workspace, notifications::DetachAndPromptErr,
};

use crate::{open_remote_project, remote_connections::restore_remote_project};

const AUTOMATIC_RECONNECT_DELAYS: [std::time::Duration; 3] = [
    std::time::Duration::ZERO,
    std::time::Duration::from_secs(2),
    std::time::Duration::from_secs(8),
];

#[derive(Clone)]
enum Host {
    CollabGuestProject,
    RemoteServerProject(RemoteConnectionOptions, bool),
}

pub struct DisconnectedOverlay {
    workspace: WeakEntity<Workspace>,
    host: Host,
    focus_handle: FocusHandle,
    finished: bool,
    automatic_reconnect_cancel: Option<oneshot::Sender<()>>,
    automatic_reconnect_failed: bool,
}

impl EventEmitter<DismissEvent> for DisconnectedOverlay {}
impl Focusable for DisconnectedOverlay {
    fn focus_handle(&self, _cx: &gpui::App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}
impl ModalView for DisconnectedOverlay {
    fn on_before_dismiss(
        &mut self,
        _window: &mut Window,
        _: &mut Context<Self>,
    ) -> workspace::DismissDecision {
        workspace::DismissDecision::Dismiss(self.finished)
    }
    fn fade_out_background(&self) -> bool {
        true
    }
}

impl DisconnectedOverlay {
    pub fn register(
        workspace: &mut Workspace,
        window: Option<&mut Window>,
        cx: &mut Context<Workspace>,
    ) {
        let Some(window) = window else {
            return;
        };
        cx.subscribe_in(
            workspace.project(),
            window,
            |workspace, project, event, window, cx| {
                if !matches!(
                    event,
                    project::Event::DisconnectedFromHost
                        | project::Event::DisconnectedFromRemote { .. }
                ) {
                    return;
                }
                let handle = cx.entity().downgrade();

                let remote_connection_options = project.read(cx).remote_connection_options(cx);
                let host = if let Some(remote_connection_options) = remote_connection_options {
                    Host::RemoteServerProject(
                        remote_connection_options,
                        matches!(
                            event,
                            project::Event::DisconnectedFromRemote {
                                server_not_running: true
                            }
                        ),
                    )
                } else {
                    Host::CollabGuestProject
                };

                let should_restore_automatically =
                    matches!(&host, Host::RemoteServerProject(_, true));
                let automatic_restore = should_restore_automatically.then(|| {
                    let app_state = workspace.app_state().clone();
                    let paths = workspace
                        .root_paths(cx)
                        .iter()
                        .map(|path| path.to_path_buf())
                        .collect::<Vec<_>>();
                    (app_state, paths)
                });
                workspace.toggle_modal(window, cx, |_, cx| DisconnectedOverlay {
                    finished: false,
                    workspace: handle,
                    host,
                    focus_handle: cx.focus_handle(),
                    automatic_reconnect_cancel: None,
                    automatic_reconnect_failed: false,
                });
                if let Some((app_state, paths)) = automatic_restore
                    && let Some(overlay) = workspace.active_modal::<DisconnectedOverlay>(cx)
                {
                    overlay.update(cx, |overlay, cx| {
                        overlay.start_automatic_reconnect(app_state, paths, window, cx)
                    });
                }
            },
        )
        .detach();
    }

    fn handle_reconnect(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_automatic_reconnect();
        self.finished = true;
        cx.emit(DismissEvent);

        if let Host::RemoteServerProject(remote_connection_options, _) = &self.host {
            self.reconnect_to_remote_project(remote_connection_options.clone(), window, cx);
        }
    }

    fn start_automatic_reconnect(
        &mut self,
        app_state: std::sync::Arc<workspace::AppState>,
        paths: Vec<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.automatic_reconnect_cancel.is_some() {
            return;
        }
        let Host::RemoteServerProject(connection_options, true) = self.host.clone() else {
            return;
        };
        let Some(window_handle) = window.window_handle().downcast::<MultiWorkspace>() else {
            return;
        };
        let source_workspace = self.workspace.clone();
        let connection_type = connection_options.connection_type();
        let (cancel_tx, cancel_rx) = oneshot::channel();
        self.automatic_reconnect_cancel = Some(cancel_tx);

        cx.spawn_in(window, async move |this, cx| {
            let cancellation = async move {
                if cancel_rx.await.is_err() {
                    futures::future::pending::<()>().await;
                }
            }
            .fuse();
            futures::pin_mut!(cancellation);
            for (attempt_index, delay) in AUTOMATIC_RECONNECT_DELAYS.into_iter().enumerate() {
                if !delay.is_zero() {
                    let timer = cx.background_executor().timer(delay).fuse();
                    futures::pin_mut!(timer);
                    futures::select_biased! {
                        _ = cancellation => return,
                        _ = timer => {},
                    }
                }
                let attempt = attempt_index + 1;
                telemetry::event!(
                    "Remote Project Automatic Reconnect",
                    connection_type,
                    attempt,
                    outcome = "started",
                );
                let result = {
                    let restore = restore_remote_project(
                        connection_options.clone(),
                        paths.clone(),
                        app_state.clone(),
                        window_handle,
                        source_workspace.clone(),
                        cx,
                    )
                    .fuse();
                    futures::pin_mut!(restore);
                    futures::select_biased! {
                        _ = cancellation => return,
                        result = restore => result,
                    }
                };
                match result {
                    Ok(()) => {
                        telemetry::event!(
                            "Remote Project Automatic Reconnect",
                            connection_type,
                            attempt,
                            outcome = "succeeded",
                        );
                        this.update(cx, |overlay, cx| {
                            overlay.finished = true;
                            overlay.automatic_reconnect_cancel.take();
                            cx.emit(DismissEvent);
                        })
                        .ok();
                        return;
                    }
                    Err(error) => {
                        log::warn!("Automatic remote reconnect failed: {error:#}");
                        telemetry::event!(
                            "Remote Project Automatic Reconnect",
                            connection_type,
                            attempt,
                            outcome = "failed",
                        );
                    }
                }
            }

            this.update(cx, |overlay, cx| {
                overlay.automatic_reconnect_cancel.take();
                overlay.automatic_reconnect_failed = true;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn cancel_automatic_reconnect(&mut self) {
        if let Some(cancel) = self.automatic_reconnect_cancel.take() {
            let _ = cancel.send(());
        }
    }

    fn reconnect_to_remote_project(
        &self,
        connection_options: RemoteConnectionOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        let Some(window_handle) = window.window_handle().downcast::<MultiWorkspace>() else {
            return;
        };

        let app_state = workspace.read(cx).app_state().clone();
        let paths = workspace
            .read(cx)
            .root_paths(cx)
            .iter()
            .map(|path| path.to_path_buf())
            .collect();

        cx.spawn_in(window, async move |_, cx| {
            open_remote_project(
                connection_options,
                paths,
                app_state,
                OpenOptions {
                    requesting_window: Some(window_handle),
                    ..Default::default()
                },
                cx,
            )
            .await?;
            Ok(())
        })
        .detach_and_prompt_err("Failed to reconnect", window, cx, |_, _, _| None);
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        self.cancel_automatic_reconnect();
        self.finished = true;
        cx.emit(DismissEvent)
    }
}

impl Render for DisconnectedOverlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let can_reconnect = matches!(self.host, Host::RemoteServerProject(..));

        let message = match &self.host {
            Host::CollabGuestProject => {
                "Your connection to the remote project has been lost.".to_string()
            }
            Host::RemoteServerProject(options, server_not_running) => {
                let autosave = if ProjectSettings::get_global(cx)
                    .session
                    .restore_unsaved_buffers
                {
                    "\nUnsaved changes are stored locally."
                } else {
                    ""
                };
                let reason = if *server_not_running {
                    if self.automatic_reconnect_failed {
                        "process exiting unexpectedly; automatic recovery was unsuccessful"
                    } else {
                        "process exiting unexpectedly; Zed is reconnecting"
                    }
                } else {
                    "not responding"
                };
                format!(
                    "Your connection to {} has been lost due to the server {reason}.{autosave}",
                    options.display_name(),
                )
            }
        };

        div()
            .track_focus(&self.focus_handle(cx))
            .elevation_3(cx)
            .on_action(cx.listener(Self::cancel))
            .occlude()
            .w(rems(24.))
            .max_h(rems(40.))
            .child(
                Modal::new("disconnected", None)
                    .header(
                        ModalHeader::new()
                            .show_dismiss_button(true)
                            .child(Headline::new("Disconnected").size(HeadlineSize::Small)),
                    )
                    .section(Section::new().child(Label::new(message)))
                    .footer(
                        ModalFooter::new().end_slot(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("close-window", "Close Window")
                                        .style(ButtonStyle::Filled)
                                        .layer(ElevationIndex::ModalSurface)
                                        .on_click(cx.listener(move |this, _, window, _| {
                                            this.cancel_automatic_reconnect();
                                            window.remove_window();
                                        })),
                                )
                                .when(can_reconnect, |el| {
                                    el.child(
                                        Button::new("reconnect", "Reconnect")
                                            .style(ButtonStyle::Filled)
                                            .layer(ElevationIndex::ModalSurface)
                                            .start_icon(Icon::new(IconName::ArrowCircle))
                                            .on_click(cx.listener(Self::handle_reconnect)),
                                    )
                                }),
                        ),
                    ),
            )
    }
}
