use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    pin::Pin,
    sync::Arc,
};

use parking_lot::RwLock;
use sdk::{Duration, Empty, GameServer, KeyValue};
use tokio::net::TcpListener;
use tonic::{async_trait, transport::server::TcpIncoming};

pub mod sdk {
    use std::hash::Hash;

    tonic::include_proto!("agones.dev.sdk");

    impl Eq for Duration {}

    impl Hash for Duration {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            self.seconds.hash(state);
        }
    }

    impl Eq for KeyValue {}

    impl Hash for KeyValue {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            self.key.hash(state);
            self.value.hash(state);
        }
    }
}

#[derive(Debug, Clone)]
enum InternalSdkCall {
    Ready,
    Allocate,
    Shutdown,
    Health(Arc<RwLock<usize>>),
    WatchGameServer,
    SetLabel(KeyValue),
    SetAnnotation(KeyValue),
    Reserve(Duration),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SdkCall {
    Ready,
    Allocate,
    Shutdown,
    Health(usize),
    WatchGameServer,
    SetLabel(KeyValue),
    SetAnnotation(KeyValue),
    Reserve(Duration),
}

impl From<&InternalSdkCall> for SdkCall {
    fn from(call: &InternalSdkCall) -> Self {
        match call {
            InternalSdkCall::Ready => Self::Ready,
            InternalSdkCall::Allocate => Self::Allocate,
            InternalSdkCall::Shutdown => Self::Shutdown,
            InternalSdkCall::Health(health) => Self::Health(*health.read()),
            InternalSdkCall::WatchGameServer => Self::WatchGameServer,
            InternalSdkCall::SetLabel(key_value) => Self::SetLabel(key_value.clone()),
            InternalSdkCall::SetAnnotation(key_value) => Self::SetAnnotation(key_value.clone()),
            InternalSdkCall::Reserve(duration) => Self::Reserve(duration.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FailableSdkCall {
    Ready,
    Allocate,
    Shutdown,
    Health,
    GetGameServer,
    WatchGameServer,
    SetLabel,
    SetAnnotation,
    Reserve,
}

impl From<&InternalSdkCall> for FailableSdkCall {
    fn from(value: &InternalSdkCall) -> Self {
        match value {
            InternalSdkCall::Ready => FailableSdkCall::Ready,
            InternalSdkCall::Allocate => FailableSdkCall::Allocate,
            InternalSdkCall::Shutdown => FailableSdkCall::Shutdown,
            InternalSdkCall::Health(_) => FailableSdkCall::Health,
            InternalSdkCall::WatchGameServer => FailableSdkCall::WatchGameServer,
            InternalSdkCall::SetLabel(_) => FailableSdkCall::SetLabel,
            InternalSdkCall::SetAnnotation(_) => FailableSdkCall::SetAnnotation,
            InternalSdkCall::Reserve(_) => FailableSdkCall::Reserve,
        }
    }
}

#[derive(Clone)]
pub struct MockAgonesServer {
    pub port: u16,
    recorded_calls: Arc<RwLock<Vec<InternalSdkCall>>>,
    stored_game_server: Arc<RwLock<GameServer>>,
    game_server_sender: flume::Sender<Result<GameServer, tonic::Status>>,
    game_server_receiver: flume::Receiver<Result<GameServer, tonic::Status>>,
    calls_to_fail: Arc<RwLock<HashMap<FailableSdkCall, tonic::Status>>>,
}

impl MockAgonesServer {
    pub async fn new() -> Self {
        Self::new_with_port(0).await
    }

    pub async fn new_with_port(port: u16) -> Self {
        let localhost_with_any_port = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
        let listener = TcpListener::bind(localhost_with_any_port).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let (game_server_sender, game_server_receiver) = flume::unbounded();

        let server = Self {
            port,
            recorded_calls: Default::default(),
            stored_game_server: Default::default(),
            game_server_sender,
            game_server_receiver,
            calls_to_fail: Default::default(),
        };

        let incoming =
            TcpIncoming::from_listener(listener, false, Some(std::time::Duration::from_secs(30)))
                .unwrap();

        tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(sdk::sdk_server::SdkServer::new(server.clone()))
                .serve_with_incoming(incoming),
        );

        server
    }

    pub fn calls(&self) -> Vec<SdkCall> {
        self.recorded_calls.read().iter().map(Into::into).collect()
    }

    pub fn game_server(&self) -> GameServer {
        self.stored_game_server.read().clone()
    }

    pub fn set_game_server(&self, gs: GameServer) {
        self.stored_game_server.write().clone_from(&gs);
        self.game_server_sender.send(Ok(gs)).unwrap();
    }

    pub fn stream_watch_game_server_error(&self, err: tonic::Status) {
        self.game_server_sender.send(Err(err)).unwrap();
    }

    pub fn fail(&self, sdk_call: FailableSdkCall, err: tonic::Status) {
        self.calls_to_fail.write().insert(sdk_call, err);
    }

    fn record(&self, call: InternalSdkCall) {
        self.recorded_calls.write().push(call);
    }

    fn fail_if_needed(&self, call: FailableSdkCall) -> Result<(), tonic::Status> {
        if let Some(err) = self.calls_to_fail.read().get(&call) {
            Err(err.clone())
        } else {
            Ok(())
        }
    }

    fn record_and_fail_if_needed(&self, call: InternalSdkCall) -> Result<(), tonic::Status> {
        let failable: FailableSdkCall = (&call).into();
        self.record(call);
        self.fail_if_needed(failable)
    }
}

#[async_trait]
impl sdk::sdk_server::Sdk for MockAgonesServer {
    /// Call when the GameServer is ready
    async fn ready(
        &self,
        _request: tonic::Request<Empty>,
    ) -> std::result::Result<tonic::Response<Empty>, tonic::Status> {
        self.record_and_fail_if_needed(InternalSdkCall::Ready)?;
        Ok(tonic::Response::new(Empty {}))
    }

    /// Call to self Allocation the GameServer
    async fn allocate(
        &self,
        _request: tonic::Request<Empty>,
    ) -> std::result::Result<tonic::Response<Empty>, tonic::Status> {
        self.record_and_fail_if_needed(InternalSdkCall::Allocate)?;
        Ok(tonic::Response::new(Empty {}))
    }

    /// Call when the GameServer is shutting down
    async fn shutdown(
        &self,
        _request: tonic::Request<Empty>,
    ) -> std::result::Result<tonic::Response<Empty>, tonic::Status> {
        self.record_and_fail_if_needed(InternalSdkCall::Shutdown)?;
        Ok(tonic::Response::new(Empty {}))
    }

    /// Send a Empty every d Duration to declare that this GameSever is healthy
    async fn health(
        &self,
        request: tonic::Request<tonic::Streaming<Empty>>,
    ) -> std::result::Result<tonic::Response<Empty>, tonic::Status> {
        let counter = Arc::new(RwLock::new(0));
        self.record_and_fail_if_needed(InternalSdkCall::Health(counter.clone()))?;

        let mut stream = request.into_inner();
        tokio::spawn(async move {
            while stream.message().await.is_ok() {
                *counter.write() += 1;
            }
        });

        Ok(tonic::Response::new(Empty {}))
    }

    /// Retrieve the current GameServer data
    async fn get_game_server(
        &self,
        _request: tonic::Request<Empty>,
    ) -> std::result::Result<tonic::Response<GameServer>, tonic::Status> {
        self.fail_if_needed(FailableSdkCall::GetGameServer)?;
        Ok(tonic::Response::new(self.game_server()))
    }

    /// Server streaming response type for the WatchGameServer method.
    type WatchGameServerStream = Pin<
        Box<
            dyn futures_core::Stream<Item = std::result::Result<GameServer, tonic::Status>>
                + Send
                + 'static,
        >,
    >;

    /// Send GameServer details whenever the GameServer is updated
    async fn watch_game_server(
        &self,
        _request: tonic::Request<Empty>,
    ) -> std::result::Result<tonic::Response<Self::WatchGameServerStream>, tonic::Status> {
        self.record_and_fail_if_needed(InternalSdkCall::WatchGameServer)?;
        let stream = self.game_server_receiver.clone().into_stream();
        Ok(tonic::Response::new(Box::pin(stream)))
    }

    /// Apply a Label to the backing GameServer metadata
    async fn set_label(
        &self,
        request: tonic::Request<KeyValue>,
    ) -> std::result::Result<tonic::Response<Empty>, tonic::Status> {
        self.record_and_fail_if_needed(InternalSdkCall::SetLabel(request.into_inner()))?;
        Ok(tonic::Response::new(Empty {}))
    }

    /// Apply a Annotation to the backing GameServer metadata
    async fn set_annotation(
        &self,
        request: tonic::Request<KeyValue>,
    ) -> std::result::Result<tonic::Response<Empty>, tonic::Status> {
        self.record_and_fail_if_needed(InternalSdkCall::SetAnnotation(request.into_inner()))?;
        Ok(tonic::Response::new(Empty {}))
    }

    /// Marks the GameServer as the Reserved state for Duration
    async fn reserve(
        &self,
        request: tonic::Request<Duration>,
    ) -> std::result::Result<tonic::Response<Empty>, tonic::Status> {
        self.record_and_fail_if_needed(InternalSdkCall::Reserve(request.into_inner()))?;
        Ok(tonic::Response::new(Empty {}))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use fake::{Fake, StringFaker};

    use crate::sdk::game_server;

    use super::*;

    fn fake_string() -> String {
        StringFaker::with((b'a'..=b'z').collect(), 4..=6).fake()
    }

    #[tokio::test]
    async fn fresh_server_has_no_calls() {
        // Arrange
        let server = MockAgonesServer::new().await;

        // Act
        agones::Sdk::new(server.port.into(), None).await.unwrap();

        // Assert
        assert_eq!(server.calls(), vec![]);
    }

    #[tokio::test]
    async fn call_to_ready_is_recorded() {
        // Arrange
        let server = MockAgonesServer::new().await;
        let mut sdk = agones::Sdk::new(server.port.into(), None).await.unwrap();

        // Act
        sdk.ready().await.unwrap();

        // Assert
        assert_eq!(server.calls(), vec![SdkCall::Ready]);
    }

    #[tokio::test]
    async fn call_to_ready_fails() {
        // Arrange
        let server = MockAgonesServer::new().await;
        let mut sdk = agones::Sdk::new(server.port.into(), None).await.unwrap();
        server.fail(
            FailableSdkCall::Ready,
            tonic::Status::internal(fake_string()),
        );

        // Act
        let result = sdk.ready().await;

        // Assert
        claims::assert_err!(result);
    }

    #[tokio::test]
    async fn failed_call_to_ready_is_recorded() {
        // Arrange
        let server = MockAgonesServer::new().await;
        let mut sdk = agones::Sdk::new(server.port.into(), None).await.unwrap();
        server.fail(
            FailableSdkCall::Ready,
            tonic::Status::internal(fake_string()),
        );

        // Act
        sdk.ready().await.unwrap_err();

        // Assert
        assert_eq!(server.calls(), vec![SdkCall::Ready]);
    }

    #[tokio::test]
    async fn call_to_allocate_is_recorded() {
        // Arrange
        let server = MockAgonesServer::new().await;
        let mut sdk = agones::Sdk::new(server.port.into(), None).await.unwrap();

        // Act
        sdk.allocate().await.unwrap();

        // Assert
        assert_eq!(server.calls(), vec![SdkCall::Allocate]);
    }

    #[tokio::test]
    async fn call_to_shutdown_is_recorded() {
        // Arrange
        let server = MockAgonesServer::new().await;
        let mut sdk = agones::Sdk::new(server.port.into(), None).await.unwrap();

        // Act
        sdk.shutdown().await.unwrap();

        // Assert
        assert_eq!(server.calls(), vec![SdkCall::Shutdown]);
    }

    #[tokio::test]
    async fn call_to_health_is_recorded() {
        // Arrange
        let server = MockAgonesServer::new().await;
        let sdk = agones::Sdk::new(server.port.into(), None).await.unwrap();
        let sender = sdk.health_check();

        // Act
        sender.send(()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Assert
        assert_eq!(server.calls(), vec![SdkCall::Health(1)]);
    }

    #[tokio::test]
    async fn call_to_get_game_server_returns_default() {
        // Arrange
        let server = MockAgonesServer::new().await;
        let mut sdk = agones::Sdk::new(server.port.into(), None).await.unwrap();

        // Act
        let game_server = sdk.get_gameserver().await.unwrap();

        // Assert
        assert_eq!(game_server, Default::default());
    }

    #[tokio::test]
    async fn call_to_get_game_server_can_be_overridden() {
        // Arrange
        let server = MockAgonesServer::new().await;
        let game_server = GameServer {
            object_meta: Some(game_server::ObjectMeta {
                name: fake_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_ne!(game_server, Default::default()); // sanity check
        server.set_game_server(game_server.clone());
        let mut sdk = agones::Sdk::new(server.port.into(), None).await.unwrap();

        // Act
        let retrieved_game_server = sdk.get_gameserver().await.unwrap();

        // Assert
        assert_eq!(
            retrieved_game_server.object_meta.unwrap().name,
            game_server.object_meta.unwrap().name
        );
    }

    #[tokio::test]
    async fn call_to_watch_game_server_is_recorded() {
        // Arrange
        let server = MockAgonesServer::new().await;
        let mut sdk = agones::Sdk::new(server.port.into(), None).await.unwrap();

        // Act
        sdk.watch_gameserver().await.unwrap();

        // Assert
        assert_eq!(server.calls(), vec![SdkCall::WatchGameServer]);
    }

    #[tokio::test]
    async fn call_to_watch_game_server_returns_updated_game_server() {
        // Arrange
        let server = MockAgonesServer::new().await;
        let mut sdk = agones::Sdk::new(server.port.into(), None).await.unwrap();
        let game_server = GameServer {
            object_meta: Some(game_server::ObjectMeta {
                name: fake_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_ne!(game_server, Default::default()); // sanity check
        let mut stream = sdk.watch_gameserver().await.unwrap();

        // Act
        server.set_game_server(game_server.clone());
        let retrieved_game_server =
            tokio::time::timeout(std::time::Duration::from_millis(100), stream.message())
                .await
                .unwrap()
                .unwrap()
                .unwrap();

        // Assert
        assert_eq!(
            retrieved_game_server.object_meta.unwrap().name,
            game_server.object_meta.unwrap().name
        );
    }

    #[tokio::test]
    async fn call_to_watch_game_server_fails_on_stream_error_call() {
        // Arrange
        let server = MockAgonesServer::new().await;
        let mut sdk = agones::Sdk::new(server.port.into(), None).await.unwrap();
        let mut stream = sdk.watch_gameserver().await.unwrap();

        // Act
        server.stream_watch_game_server_error(tonic::Status::internal(fake_string()));
        let result = tokio::time::timeout(std::time::Duration::from_millis(100), stream.message())
            .await
            .unwrap();

        // Assert
        claims::assert_err!(result);
    }

    #[tokio::test]
    async fn call_to_set_label_is_recorded() {
        // Arrange
        let server = MockAgonesServer::new().await;
        let mut sdk = agones::Sdk::new(server.port.into(), None).await.unwrap();
        let key = fake_string();
        let value = fake_string();

        // Act
        sdk.set_label(&key, &value).await.unwrap();

        // Assert
        assert_eq!(
            server.calls(),
            vec![SdkCall::SetLabel(KeyValue { key, value })]
        );
    }

    #[tokio::test]
    async fn call_to_set_annotation_is_recorded() {
        // Arrange
        let server = MockAgonesServer::new().await;
        let mut sdk = agones::Sdk::new(server.port.into(), None).await.unwrap();
        let key = fake_string();
        let value = fake_string();

        // Act
        sdk.set_annotation(&key, &value).await.unwrap();

        // Assert
        assert_eq!(
            server.calls(),
            vec![SdkCall::SetAnnotation(KeyValue { key, value })]
        );
    }

    #[tokio::test]
    async fn call_to_reserve_is_recorded() {
        // Arrange
        let server = MockAgonesServer::new().await;
        let mut sdk = agones::Sdk::new(server.port.into(), None).await.unwrap();
        let seconds = (0..1000).fake();
        let duration = std::time::Duration::from_secs(seconds);

        // Act
        sdk.reserve(duration.clone()).await.unwrap();

        // Assert
        assert_eq!(
            server.calls(),
            vec![SdkCall::Reserve(sdk::Duration {
                seconds: seconds as i64
            })]
        );
    }
}
