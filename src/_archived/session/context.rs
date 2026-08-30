use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use thiserror::Error;
use tokio::sync::Notify;
use tokio::time::Instant;

use crate::gpio::GPIOCluster;
use crate::gpio::GPIOClusterError;
use crate::gpio::GPIOPinConfig;
use crate::gpio::PinLevel;
use crate::protocol::request::BiasMode;
use crate::protocol::request::DriveMode;
use crate::protocol::request::PinSelector;
use crate::protocol::request::TargetConfigRequest;
use crate::protocol::response::ResponseMessage;

use super::cache::TargetsCache;
use super::cache::TargetsCacheError;
use super::state::SessionState;

#[derive(Debug, Error)]
pub enum SessionStateError {
    #[error("session is already initialized")]
    AlreadyInitialized,
    #[error("session is not initialized")]
    NotInitialized,
    #[error("session is closed")]
    Closed,
    #[error("a set request is already in progress")]
    SetInProgress,
}


#[derive(Debug)]
pub struct InitializedSession<C>
where
    C: GPIOCluster,
{
    pub init_request_id: String,
    pub cluster: C,
    pub cache: TargetsCache,
}


#[derive(Debug)]
pub struct SessionContext<C>
where
    C: GPIOCluster,
{
    initialized: Option<Arc<InitializedSession<C>>>,
    setting: bool,
}

impl<C> SessionContext<C>
where
    C: GPIOCluster,
{
    pub fn new() -> Self {
        Self {
            initialized: None,
            setting: false,
        }
    }
    
    pub fn enter_init<'a>(&'a mut self) -> Result<InitGuard<'a, C>, SessionStateError> {
        if self.initialized.is_some() {
            return Err(SessionStateError::AlreadyInitialized);
        }
        Ok(InitGuard { initialized: &mut self.initialized })
    }

    pub fn enter_get(&self) -> Result<GetGuard<C>, SessionStateError> {
        let context = if let Some(context) = self.initialized.as_ref() {
            context.clone()
        } else {
            return Err(SessionStateError::NotInitialized);
        };
        Ok(GetGuard { context })
    }

    pub fn enter_set<'a>(&'a mut self) -> Result<SetGuard<'a, C>, SessionStateError> {
        let context = if let Some(context) = self.initialized.as_ref() {
            context.clone()
        } else {
            return Err(SessionStateError::NotInitialized);
        };
        self.setting = true;
        Ok(SetGuard { setting: &mut self.setting, context })
    }
}


pub struct InitGuard<'a, C>
where
    C: GPIOCluster,
{
    initialized: &'a mut Option<Arc<InitializedSession<C>>>,
}

impl<'a, C> InitGuard<'a, C>
where
    C: GPIOCluster,
{
    pub fn initialize<'b>(self, request_id: String, cluster: C, request: &'b BTreeMap<String, TargetConfigRequest>) -> Result<(), TargetsCacheError<'b>> {
        let cache = TargetsCache::build(&cluster, request)?;
        *self.initialized = Some(
            Arc::new(
                InitializedSession {
                    init_request_id: request_id,
                    cluster,
                    cache,
                }
            )
        );
        Ok(())
    }
}


pub struct GetGuard<C>
where
    C: GPIOCluster,
{
    context: Arc<InitializedSession<C>>,
}

impl<'a, C> GetGuard<'a, C>
where
    C: GPIOCluster,
{
    pub fn context(&self) -> Arc<InitializedSession<C>> {
        self.context.clone()
    }
}


/// RAII guard that marks the session [`SessionState::SetInProgress`] dropped.
pub struct SetGuard<'a, C>
where
    C: GPIOCluster,
{
    setting: &'a mut bool,
    context: Arc<InitializedSession<C>>,
}

impl <'a, C> SetGuard<'a, C>
where
    C: GPIOCluster,
{
    pub fn context(&self) -> Arc<InitializedSession<C>> {
        self.context.clone()
    }
}

impl<C> Drop for SetGuard<'_, C>
where
    C: GPIOCluster,
{
    fn drop(&mut self) {
        *self.setting = false;
    }
}





#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::time::Instant;

    use crate::gpio::mock::MockBackend;
    use crate::protocol::request::RequestMessage;
    use crate::protocol::request::RequestPayload;
    use crate::protocol::request::SetTargetValue;
    use crate::protocol::response::PinValuePayload;
    use crate::protocol::response::ResponseStatus;

    use super::*;

    fn temp_state(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("gpiochips.xml");
        fs::write(&path, content).expect("write state");
        (dir, path)
    }

    fn sample_request(line: &str) -> RequestMessage {
        let line: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        crate::protocol::parse_request_line(&line).expect("request should parse")
    }

    const SAMPLE_XML: &str = r#"
<gpiochips>
    <gpiochip id="gpiochip0">
        <line id="0" mode="input" bias="pull_up">1</line>
        <line id="1" mode="input" bias="pull_down">0</line>
        <line id="2" mode="output" drive="push_pull">0</line>
        <line id="3" mode="output" drive="push_pull">1</line>
        <line id="4" mode="input" event="both">0</line>
    </gpiochip>
</gpiochips>
"#;

    // #[tokio::test]
    // async fn handle_request_rejects_get_before_init() {
    //     let (_dir, path) = temp_state(SAMPLE_XML);
    //     let backend = MockBackend::new(&path);
    //     let mut session = SessionContext::new();

    //     let response = handle_request(
    //         &mut session,
    //         &backend,
    //         sample_request(r#"{"id":"get-1","action":"get","target":"IN"}"#),
    //     )
    //     .await;

    //     assert_eq!(response.id, "get-1");
    //     assert!(matches!(response.status, ResponseStatus::Error { .. }));
    // }

    // #[tokio::test]
    // async fn handle_request_initializes_and_reads_targets() {
    //     let (_dir, path) = temp_state(SAMPLE_XML);
    //     let backend = MockBackend::new(&path);
    //     let mut session = SessionContext::new();

    //     let init = sample_request(
    //         r#"{
    //             "id":"init-1",
    //             "action":"init",
    //             "target":{
    //                 "IN":{"mode":"input","pin":"gpiochip0:0"},
    //                 "OUT2":{"mode":"output","pin":["gpiochip0:2","gpiochip0:3"]},
    //                 "TRIG":{"mode":"trigger","pin":"gpiochip0:4","edge":"both"}
    //             }
    //         }"#,
    //     );
    //     let init_response = handle_request(&mut session, &backend, init).await;
    //     assert!(matches!(init_response.status, ResponseStatus::Ok));
    //     assert_eq!(session.state(), SessionState::Initialized);

    //     let get = sample_request(r#"{"id":"get-1","action":"get","target":"IN"}"#);
    //     let response = handle_request(&mut session, &backend, get).await;
    //     match response.status {
    //         ResponseStatus::PinValue {
    //             pin_value: PinValuePayload::Value(value),
    //         } => assert_eq!(value, 1),
    //         other => panic!("expected pin value, got {other:?}"),
    //     }

    //     let get_output = sample_request(r#"{"id":"get-2","action":"get","target":"OUT2"}"#);
    //     let response = handle_request(&mut session, &backend, get_output).await;
    //     assert!(matches!(response.status, ResponseStatus::Error { .. }));
    // }

    // #[tokio::test]
    // async fn handle_request_sets_output_values() {
    //     let (_dir, path) = temp_state(SAMPLE_XML);
    //     let backend = MockBackend::new(&path);
    //     let mut session = SessionContext::new();

    //     let init = sample_request(
    //         r#"{
    //             "id":"init-1",
    //             "action":"init",
    //             "target":{
    //                 "OUT2":{"mode":"output","pin":["gpiochip0:2","gpiochip0:3"]}
    //             }
    //         }"#,
    //     );
    //     let _ = handle_request(&mut session, &backend, init).await;

    //     let set = sample_request(r#"{"id":"set-1","action":"set","target":{"OUT2":2}}"#);
    //     let response = handle_request(&mut session, &backend, set).await;
    //     assert!(matches!(response.status, ResponseStatus::Ok));

    //     let get = sample_request(r#"{"id":"get-1","action":"get","target":"OUT2"}"#);
    //     let response = handle_request(&mut session, &backend, get).await;
    //     assert!(matches!(response.status, ResponseStatus::Error { .. }));
    // }

    // #[tokio::test]
    // async fn handle_request_executes_multi_step_set_sequence() {
    //     let (_dir, path) = temp_state(SAMPLE_XML);
    //     let backend = MockBackend::new(&path);
    //     let mut session = SessionContext::new();

    //     let init = sample_request(
    //         r#"{
    //             "id":"init-1",
    //             "action":"init",
    //             "target":{"OUT":{"mode":"output","pin":"gpiochip0:2"}}
    //         }"#,
    //     );
    //     let _ = handle_request(&mut session, &backend, init).await;

    //     let set = sample_request(
    //         r#"{
    //             "id":"set-1",
    //             "action":"set",
    //             "target":{"OUT":[{"value":1},{"lag":10,"value":0}]}
    //         }"#,
    //     );
    //     let response = handle_request(&mut session, &backend, set).await;
    //     assert!(matches!(response.status, ResponseStatus::Ok));
    //     assert_eq!(session.state(), SessionState::Initialized);
    // }

    // #[tokio::test]
    // async fn schedule_wait_errors_when_scheduling_never_enabled() {
    //     let mut session = SessionContext::<crate::gpio::mock::MockCluster>::new();
    //     let result = session
    //         .schedule_wait(Instant::now() + Duration::from_millis(10))
    //         .await;
    //     assert!(matches!(result, Err(SessionError::ScheduleDisabled)));
    // }

    // #[tokio::test]
    // async fn schedule_wait_errors_immediately_when_disabled() {
    //     let mut session = SessionContext::<crate::gpio::mock::MockCluster>::new();
    //     session.enable_schedule().expect("enable");
    //     session.disable_schedule();

    //     let result = session
    //         .schedule_wait(Instant::now() + Duration::from_millis(50))
    //         .await;
    //     assert!(matches!(result, Err(SessionError::ScheduleDisabled)));
    // }

    // #[tokio::test]
    // async fn schedule_wait_completes_when_target_time_reached() {
    //     let mut session = SessionContext::<crate::gpio::mock::MockCluster>::new();
    //     session.enable_schedule().expect("enable");

    //     let target = Instant::now() + Duration::from_millis(20);
    //     let started = Instant::now();
    //     session.schedule_wait(target).await.expect("wait");
    //     assert!(started.elapsed() >= Duration::from_millis(15));
    // }

    // #[tokio::test]
    // async fn schedule_wait_interrupted_by_disable() {
    //     let control = super::ScheduleControl::new();
    //     let control_disable = Arc::clone(&control);
    //     let target = Instant::now() + Duration::from_secs(5);

    //     let waiter = tokio::spawn(async move {
    //         crate::session::context::schedule_wait_on_control::<crate::gpio::mock::MockError>(
    //             control, target,
    //         )
    //         .await
    //     });

    //     tokio::time::sleep(Duration::from_millis(20)).await;
    //     control_disable.disable();

    //     let result = waiter.await.expect("join");
    //     assert!(matches!(result, Err(SessionError::ScheduleDisabled)));
    // }

    // #[tokio::test]
    // async fn close_disables_scheduling_before_cluster_cleanup() {
    //     let (_dir, path) = temp_state(SAMPLE_XML);
    //     let backend = MockBackend::new(&path);
    //     let mut session = SessionContext::new();

    //     let init = sample_request(
    //         r#"{
    //             "id":"init-1",
    //             "action":"init",
    //             "target":{"OUT":{"mode":"output","pin":"gpiochip0:2"}}
    //         }"#,
    //     );
    //     let _ = handle_request(&mut session, &backend, init).await;
    //     session.enable_schedule().expect("enable");

    //     session.close().await.expect("close");
    //     assert!(matches!(
    //         session
    //             .schedule_wait(Instant::now() + Duration::from_millis(10))
    //             .await,
    //         Err(SessionError::ScheduleDisabled)
    //     ));
    // }

    // #[tokio::test]
    // async fn begin_set_holds_set_in_progress_until_guard_dropped() {
    //     let (_dir, path) = temp_state(SAMPLE_XML);
    //     let backend = MockBackend::new(&path);
    //     let mut session = SessionContext::new();

    //     let init = sample_request(
    //         r#"{
    //             "id":"init-1",
    //             "action":"init",
    //             "target":{"OUT":{"mode":"output","pin":"gpiochip0:2"}}
    //         }"#,
    //     );
    //     let _ = handle_request(&mut session, &backend, init).await;

    //     let guard = session.begin_set().expect("begin set");
    //     assert_eq!(guard.state(), SessionState::SetInProgress);
    //     drop(guard);
    //     assert_eq!(session.state(), SessionState::Initialized);
    // }

    // #[tokio::test]
    // async fn begin_set_rejects_second_set_while_in_progress() {
    //     let (_dir, path) = temp_state(SAMPLE_XML);
    //     let backend = MockBackend::new(&path);
    //     let mut session = SessionContext::new();

    //     let init = sample_request(
    //         r#"{
    //             "id":"init-1",
    //             "action":"init",
    //             "target":{"OUT":{"mode":"output","pin":"gpiochip0:2"}}
    //         }"#,
    //     );
    //     let _ = handle_request(&mut session, &backend, init).await;

    //     session.force_state_for_test(SessionState::SetInProgress);
    //     assert!(matches!(
    //         session.begin_set(),
    //         Err(SessionError::SetInProgress)
    //     ));
    // }

    // #[tokio::test]
    // async fn set_guard_forwards_schedule_control() {
    //     let (_dir, path) = temp_state(SAMPLE_XML);
    //     let backend = MockBackend::new(&path);
    //     let mut session = SessionContext::new();

    //     let init = sample_request(
    //         r#"{
    //             "id":"init-1",
    //             "action":"init",
    //             "target":{"OUT":{"mode":"output","pin":"gpiochip0:2"}}
    //         }"#,
    //     );
    //     let _ = handle_request(&mut session, &backend, init).await;

    //     let mut guard = session.begin_set().expect("begin set");
    //     guard.enable_schedule().expect("enable");
    //     guard
    //         .schedule_wait(Instant::now() + Duration::from_millis(10))
    //         .await
    //         .expect("wait");
    //     guard.disable_schedule();
    //     drop(guard);
    //     assert_eq!(session.state(), SessionState::Initialized);
    // }

    // #[tokio::test]
    // async fn enable_schedule_replaces_stale_control_and_wakes_old_waiters() {
    //     let mut session = SessionContext::<crate::gpio::mock::MockCluster>::new();
    //     session.enable_schedule().expect("first enable");
    //     let stale = session
    //         .active_schedule_control_for_test()
    //         .expect("stale control");

    //     let target = Instant::now() + Duration::from_secs(5);
    //     let waiter = tokio::spawn(async move {
    //         schedule_wait_on_control::<crate::gpio::mock::MockError>(stale, target).await
    //     });

    //     tokio::time::sleep(Duration::from_millis(20)).await;
    //     session.enable_schedule().expect("replace stale control");

    //     let result = waiter.await.expect("join");
    //     assert!(matches!(result, Err(SessionError::ScheduleDisabled)));
    // }

    // #[tokio::test]
    // async fn close_interrupts_active_schedule_wait() {
    //     let (_dir, path) = temp_state(SAMPLE_XML);
    //     let backend = MockBackend::new(&path);
    //     let mut session = SessionContext::new();

    //     let init = sample_request(
    //         r#"{
    //             "id":"init-1",
    //             "action":"init",
    //             "target":{"OUT":{"mode":"output","pin":"gpiochip0:2"}}
    //         }"#,
    //     );
    //     let _ = handle_request(&mut session, &backend, init).await;
    //     session.enable_schedule().expect("enable");
    //     let control = session
    //         .active_schedule_control_for_test()
    //         .expect("active control");

    //     let target = Instant::now() + Duration::from_secs(5);
    //     let waiter = tokio::spawn(async move {
    //         schedule_wait_on_control::<crate::gpio::mock::MockError>(control, target).await
    //     });

    //     tokio::time::sleep(Duration::from_millis(20)).await;
    //     session.close().await.expect("close");

    //     let result = waiter.await.expect("join");
    //     assert!(matches!(result, Err(SessionError::ScheduleDisabled)));
    // }

    // #[tokio::test]
    // async fn wait_for_event_maps_cluster_event_back_to_init_target() {
    //     let (_dir, path) = temp_state(SAMPLE_XML);
    //     let backend = MockBackend::new(&path);
    //     let mut session = SessionContext::new();

    //     let init = sample_request(
    //         r#"{
    //             "id":"init-1",
    //             "action":"init",
    //             "target":{"TRIG":{"mode":"trigger","pin":"gpiochip0:4","edge":"both"}}
    //         }"#,
    //     );
    //     let _ = handle_request(&mut session, &backend, init).await;
    //     let session = Arc::new(session);

    //     tokio::time::sleep(Duration::from_millis(50)).await;
    //     let waiter = {
    //         let session = Arc::clone(&session);
    //         tokio::spawn(async move { wait_for_event(&session).await.expect("event") })
    //     };

    //     fs::write(
    //         &path,
    //         r#"<gpiochips><gpiochip id="gpiochip0"><line id="0" mode="input" bias="pull_up">1</line><line id="1" mode="input" bias="pull_down">0</line><line id="2" mode="output" drive="push_pull">0</line><line id="3" mode="output" drive="push_pull">1</line><line id="4" mode="input" event="both">1</line></gpiochip></gpiochips>"#,
    //     )
    //     .expect("update trigger line");

    //     let response = tokio::time::timeout(Duration::from_secs(1), waiter)
    //         .await
    //         .expect("event timeout")
    //         .expect("join");
    //     assert_eq!(response.id, "init-1");
    //     match response.status {
    //         ResponseStatus::Event { event } => {
    //             assert_eq!(event.target, "TRIG");
    //             assert_eq!(event.kind, crate::protocol::response::EventType::Rising);
    //         }
    //         other => panic!("expected event response, got {other:?}"),
    //     }
    // }

    // #[test]
    // fn set_target_value_sequence_parses_as_sequence() {
    //     let request = sample_request(
    //         r#"{
    //             "id":"set-1",
    //             "action":"set",
    //             "target":{"OUT":[{"value":1},{"lag":10,"value":0}]}
    //         }"#,
    //     );

    //     match request.payload {
    //         RequestPayload::Set { target } => match target.get("OUT") {
    //             Some(SetTargetValue::Sequence(steps)) => assert_eq!(steps.len(), 2),
    //             other => panic!("expected sequence value, got {other:?}"),
    //         },
    //         other => panic!("expected set request, got {other:?}"),
    //     }
    // }
}
