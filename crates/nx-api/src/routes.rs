use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use nx_core::{ControlError, ModuleInfo, PeerInfo, SharedRuntimeControl};
use serde::{Deserialize, Serialize};

use crate::{error_response, payload_too_large_response};

const DEFAULT_PAGE_SIZE: usize = 50;
const MAX_PAGE_SIZE: usize = 100;
const MAX_KEY_SIZE: usize = 8 * 1024;
const MAX_RESPONSE_BODY_SIZE: usize = 1024 * 1024;

#[derive(Clone)]
struct ApiState {
    control: SharedRuntimeControl,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PageQuery {
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyPageQuery {
    prefix: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Serialize)]
struct ModuleResponse {
    id: String,
    size_bytes: u64,
}

impl From<ModuleInfo> for ModuleResponse {
    fn from(module: ModuleInfo) -> Self {
        Self {
            id: module.id,
            size_bytes: module.size_bytes,
        }
    }
}

#[derive(Serialize)]
struct PeerResponse {
    address: String,
    node_id: String,
}

impl From<PeerInfo> for PeerResponse {
    fn from(peer: PeerInfo) -> Self {
        Self {
            address: peer.address,
            node_id: peer.node_id,
        }
    }
}

#[derive(Serialize)]
struct PageResponse<T> {
    items: Vec<T>,
    next_cursor: Option<String>,
}

#[derive(Serialize)]
struct ProbeResponse {
    status: &'static str,
}

pub(crate) fn router(control: SharedRuntimeControl) -> Router {
    Router::new()
        .route("/api/v1/modules", post(register_module).get(list_modules))
        .route(
            "/api/v1/modules/{id}",
            get(get_module).delete(delete_module),
        )
        .route("/api/v1/modules/{id}/runs", post(run_module))
        .route("/api/v1/peers", get(list_peers))
        .route("/api/v1/keys", get(list_keys))
        .route("/api/v1/keys/{key}", get(get_value))
        .route("/api/v1/health", get(health))
        .route("/api/v1/ready", get(ready))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(ApiState { control })
}

async fn register_module(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    if !is_wasm_content_type(&headers) {
        return error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "content type must be application/wasm",
        );
    }

    let body = match body {
        Ok(body) => body,
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return payload_too_large_response();
        }
        Err(_) => return invalid_request(),
    };
    if body.is_empty() {
        return invalid_request();
    }

    match state.control.register_module(body.to_vec()).await {
        Ok(registration) => {
            let status = if registration.created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            let id = registration.module.id.clone();
            let mut response =
                (status, Json(ModuleResponse::from(registration.module))).into_response();
            if status == StatusCode::CREATED {
                let location = format!("/api/v1/modules/{id}");
                // The identifier is lowercase hexadecimal, therefore always a valid header value.
                response.headers_mut().insert(
                    header::LOCATION,
                    HeaderValue::from_str(&location)
                        .expect("module location must be a valid header"),
                );
            }
            response
        }
        Err(error) => control_error_response(error),
    }
}

async fn list_modules(
    State(state): State<ApiState>,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Response {
    let query = match parse_page_query(query, CursorKind::Module) {
        Ok(query) => query,
        Err(_) => return invalid_request(),
    };
    match state.control.list_modules(query.cursor, query.limit).await {
        Ok(page) => Json(PageResponse {
            items: page.items.into_iter().map(ModuleResponse::from).collect(),
            next_cursor: encode_cursor(page.next_cursor),
        })
        .into_response(),
        Err(error) => control_error_response(error),
    }
}

async fn get_module(
    State(state): State<ApiState>,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let id = match path_value(path) {
        Ok(id) => id,
        Err(_) => return invalid_request(),
    };
    if !is_module_id(&id) {
        return invalid_request();
    }
    match state.control.get_module(id).await {
        Ok(module) => Json(ModuleResponse::from(module)).into_response(),
        Err(error) => control_error_response(error),
    }
}

async fn delete_module(
    State(state): State<ApiState>,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let id = match path_value(path) {
        Ok(id) => id,
        Err(_) => return invalid_request(),
    };
    if !is_module_id(&id) {
        return invalid_request();
    }
    match state.control.delete_module(id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => control_error_response(error),
    }
}

async fn run_module(
    State(state): State<ApiState>,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let id = match path_value(path) {
        Ok(id) => id,
        Err(_) => return invalid_request(),
    };
    if !is_module_id(&id) {
        return invalid_request();
    }
    match state.control.run_module(id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => control_error_response(error),
    }
}

async fn list_peers(
    State(state): State<ApiState>,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Response {
    let query = match parse_page_query(query, CursorKind::Peer) {
        Ok(query) => query,
        Err(_) => return invalid_request(),
    };
    match state.control.list_peers(query.cursor, query.limit).await {
        Ok(page) => Json(PageResponse {
            items: page.items.into_iter().map(PeerResponse::from).collect(),
            next_cursor: encode_cursor(page.next_cursor),
        })
        .into_response(),
        Err(error) => control_error_response(error),
    }
}

async fn list_keys(
    State(state): State<ApiState>,
    query: Result<Query<KeyPageQuery>, QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(query) => query,
        Err(_) => return invalid_request(),
    };
    let limit = match page_size(query.limit) {
        Ok(limit) => limit,
        Err(_) => return invalid_request(),
    };
    let prefix = match decode_key(query.prefix.as_deref(), true) {
        Ok(prefix) => prefix,
        Err(_) => return invalid_request(),
    };
    let cursor = match query
        .cursor
        .as_deref()
        .map(|cursor| decode_key(Some(cursor), false))
        .transpose()
    {
        Ok(cursor) => cursor,
        Err(_) => return invalid_request(),
    };

    match state.control.list_keys(prefix, cursor, limit).await {
        Ok(page) => bounded_key_page(page.items, page.next_cursor),
        Err(error) => control_error_response(error),
    }
}

async fn get_value(
    State(state): State<ApiState>,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let key = match path_value(path) {
        Ok(key) => key,
        Err(_) => return invalid_request(),
    };
    let key_bytes = match decode_key(Some(&key), false) {
        Ok(key) => key,
        Err(_) => return invalid_request(),
    };
    match state.control.get_value(key_bytes).await {
        Ok(value) if value.len() <= MAX_RESPONSE_BODY_SIZE => {
            let mut response = value.into_response();
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
            response.headers_mut().insert(
                HeaderName::from_static("x-numax-key"),
                HeaderValue::from_str(&key).expect("Base64URL key must be a valid header value"),
            );
            response
        }
        Ok(_) => internal_error(),
        Err(error) => control_error_response(error),
    }
}

async fn health(State(state): State<ApiState>) -> Response {
    if state.control.is_healthy() {
        Json(ProbeResponse { status: "healthy" }).into_response()
    } else {
        internal_error()
    }
}

async fn ready(State(state): State<ApiState>) -> Response {
    if state.control.is_ready() {
        Json(ProbeResponse { status: "ready" }).into_response()
    } else {
        let mut response = error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "not_ready",
            "node is not ready",
        );
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        response
    }
}

async fn not_found() -> Response {
    error_response(StatusCode::NOT_FOUND, "not_found", "resource not found")
}

async fn method_not_allowed() -> Response {
    error_response(
        StatusCode::METHOD_NOT_ALLOWED,
        "invalid_request",
        "method not allowed",
    )
}

#[derive(Clone, Copy)]
enum CursorKind {
    Module,
    Peer,
}

struct ParsedPageQuery {
    cursor: Option<Vec<u8>>,
    limit: usize,
}

#[derive(Clone, Copy)]
struct InvalidRequest;

fn parse_page_query(
    query: Result<Query<PageQuery>, QueryRejection>,
    kind: CursorKind,
) -> Result<ParsedPageQuery, InvalidRequest> {
    let Query(query) = query.map_err(|_| InvalidRequest)?;
    let limit = page_size(query.limit)?;
    let cursor = query.cursor.as_deref().map(decode_cursor).transpose()?;
    if let Some(cursor) = cursor.as_deref() {
        match kind {
            CursorKind::Module if !is_module_id_bytes(cursor) => return Err(InvalidRequest),
            CursorKind::Peer if std::str::from_utf8(cursor).is_err() => {
                return Err(InvalidRequest);
            }
            _ => {}
        }
    }
    Ok(ParsedPageQuery { cursor, limit })
}

fn page_size(limit: Option<usize>) -> Result<usize, InvalidRequest> {
    let limit = limit.unwrap_or(DEFAULT_PAGE_SIZE);
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(InvalidRequest);
    }
    Ok(limit)
}

fn path_value(path: Result<Path<String>, PathRejection>) -> Result<String, InvalidRequest> {
    path.map(|Path(value)| value).map_err(|_| InvalidRequest)
}

fn decode_cursor(cursor: &str) -> Result<Vec<u8>, InvalidRequest> {
    if cursor.is_empty() || cursor.len() > 16_384 {
        return Err(InvalidRequest);
    }
    URL_SAFE_NO_PAD.decode(cursor).map_err(|_| InvalidRequest)
}

fn encode_cursor(cursor: Option<Vec<u8>>) -> Option<String> {
    cursor.map(|cursor| URL_SAFE_NO_PAD.encode(cursor))
}

fn decode_key(encoded: Option<&str>, allow_absent: bool) -> Result<Vec<u8>, InvalidRequest> {
    let Some(encoded) = encoded else {
        return if allow_absent {
            Ok(Vec::new())
        } else {
            Err(InvalidRequest)
        };
    };
    if encoded.is_empty() {
        return Err(InvalidRequest);
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| InvalidRequest)?;
    if decoded.is_empty() || decoded.len() > MAX_KEY_SIZE {
        return Err(InvalidRequest);
    }
    Ok(decoded)
}

fn bounded_key_page(items: Vec<Vec<u8>>, next_cursor: Option<Vec<u8>>) -> Response {
    if items
        .iter()
        .any(|key| key.is_empty() || key.len() > MAX_KEY_SIZE)
    {
        return internal_error();
    }
    let mut encoded_items = items
        .into_iter()
        .map(|key| URL_SAFE_NO_PAD.encode(key))
        .collect::<Vec<_>>();
    let mut next_cursor = encode_cursor(next_cursor);

    loop {
        let page = PageResponse {
            items: encoded_items.clone(),
            next_cursor: next_cursor.clone(),
        };
        match serde_json::to_vec(&page) {
            Ok(bytes) if bytes.len() <= MAX_RESPONSE_BODY_SIZE => {
                return Json(page).into_response();
            }
            Ok(_) if !encoded_items.is_empty() => {
                encoded_items.pop();
                next_cursor = encoded_items.last().cloned();
            }
            Ok(_) | Err(_) => return internal_error(),
        }
    }
}

fn is_wasm_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/wasm"))
}

fn is_module_id(id: &str) -> bool {
    is_module_id_bytes(id.as_bytes())
}

fn is_module_id_bytes(id: &[u8]) -> bool {
    id.len() == 64
        && id
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn control_error_response(error: ControlError) -> Response {
    match error {
        ControlError::InvalidCursor => invalid_request(),
        ControlError::ModuleNotFound => {
            error_response(StatusCode::NOT_FOUND, "not_found", "module not found")
        }
        ControlError::KeyNotFound => {
            error_response(StatusCode::NOT_FOUND, "not_found", "key not found")
        }
        ControlError::InvalidModule(_) => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "module_invalid",
            "invalid WebAssembly module",
        ),
        ControlError::ModuleExecutionFailed(_) => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "module_execution_failed",
            "module execution failed",
        ),
        ControlError::Storage(error) => {
            tracing::error!(%error, "management operation failed in storage");
            internal_error()
        }
        ControlError::Internal(error) => {
            tracing::error!(%error, "management operation failed internally");
            internal_error()
        }
    }
}

fn invalid_request() -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "invalid request",
    )
}

fn internal_error() -> Response {
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "internal server error",
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use http_body_util::BodyExt;
    use nx_core::{ControlPage, ModuleRegistration, RuntimeIntrospection, RuntimeManagement};
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;

    #[derive(Default)]
    struct TestControl {
        modules: Mutex<BTreeMap<String, ModuleInfo>>,
        ready: AtomicBool,
    }

    impl TestControl {
        fn module_id(bytes: &[u8]) -> String {
            format!("{:064x}", bytes.len())
        }
    }

    #[async_trait]
    impl RuntimeIntrospection for TestControl {
        async fn list_modules(
            &self,
            cursor: Option<Vec<u8>>,
            limit: usize,
        ) -> Result<ControlPage<ModuleInfo>, ControlError> {
            let cursor = cursor
                .map(String::from_utf8)
                .transpose()
                .map_err(|_| ControlError::InvalidCursor)?;
            let mut items = self
                .modules
                .lock()
                .unwrap()
                .values()
                .filter(|module| cursor.as_ref().is_none_or(|cursor| &module.id > cursor))
                .take(limit + 1)
                .cloned()
                .collect::<Vec<_>>();
            let has_more = items.len() > limit;
            items.truncate(limit);
            let next_cursor = has_more.then(|| items.last().unwrap().id.as_bytes().to_vec());
            Ok(ControlPage { items, next_cursor })
        }

        async fn get_module(&self, id: String) -> Result<ModuleInfo, ControlError> {
            self.modules
                .lock()
                .unwrap()
                .get(&id)
                .cloned()
                .ok_or(ControlError::ModuleNotFound)
        }

        async fn list_peers(
            &self,
            cursor: Option<Vec<u8>>,
            limit: usize,
        ) -> Result<ControlPage<PeerInfo>, ControlError> {
            let cursor = cursor
                .map(String::from_utf8)
                .transpose()
                .map_err(|_| ControlError::InvalidCursor)?;
            let mut items = [
                PeerInfo {
                    address: "127.0.0.1:9001".to_string(),
                    node_id: "node-a".to_string(),
                },
                PeerInfo {
                    address: "127.0.0.1:9002".to_string(),
                    node_id: "node-b".to_string(),
                },
            ]
            .into_iter()
            .filter(|peer| cursor.as_ref().is_none_or(|cursor| &peer.address > cursor))
            .take(limit + 1)
            .collect::<Vec<_>>();
            let has_more = items.len() > limit;
            items.truncate(limit);
            let next_cursor = has_more.then(|| items.last().unwrap().address.as_bytes().to_vec());
            Ok(ControlPage { items, next_cursor })
        }

        async fn list_keys(
            &self,
            prefix: Vec<u8>,
            cursor: Option<Vec<u8>>,
            limit: usize,
        ) -> Result<ControlPage<Vec<u8>>, ControlError> {
            let mut items = [b"app:a".to_vec(), b"app:b".to_vec(), vec![0, 255, b'k']]
                .into_iter()
                .filter(|key| key.starts_with(&prefix))
                .filter(|key| cursor.as_ref().is_none_or(|cursor| key > cursor))
                .take(limit + 1)
                .collect::<Vec<_>>();
            items.sort();
            let has_more = items.len() > limit;
            items.truncate(limit);
            let next_cursor = has_more.then(|| items.last().unwrap().clone());
            Ok(ControlPage { items, next_cursor })
        }

        async fn get_value(&self, key: Vec<u8>) -> Result<Vec<u8>, ControlError> {
            match key.as_slice() {
                [0, 255, b'k'] => Ok(vec![0, 1, 255]),
                _ => Err(ControlError::KeyNotFound),
            }
        }

        fn is_healthy(&self) -> bool {
            true
        }

        fn is_ready(&self) -> bool {
            self.ready.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl RuntimeManagement for TestControl {
        async fn register_module(
            &self,
            bytes: Vec<u8>,
        ) -> Result<ModuleRegistration, ControlError> {
            if bytes == b"invalid" {
                return Err(ControlError::InvalidModule("test rejection".to_string()));
            }
            let id = Self::module_id(&bytes);
            let module = ModuleInfo {
                id: id.clone(),
                size_bytes: bytes.len() as u64,
            };
            let created = self
                .modules
                .lock()
                .unwrap()
                .insert(id, module.clone())
                .is_none();
            Ok(ModuleRegistration { module, created })
        }

        async fn delete_module(&self, id: String) -> Result<(), ControlError> {
            self.modules.lock().unwrap().remove(&id);
            Ok(())
        }

        async fn run_module(&self, id: String) -> Result<(), ControlError> {
            if id == "f".repeat(64) {
                return Err(ControlError::ModuleExecutionFailed("test trap".to_string()));
            }
            self.modules
                .lock()
                .unwrap()
                .contains_key(&id)
                .then_some(())
                .ok_or(ControlError::ModuleNotFound)
        }
    }

    fn app(control: Arc<TestControl>) -> Router {
        router(control)
    }

    async fn call(
        app: Router,
        method: Method,
        uri: &str,
        content_type: Option<&str>,
        body: impl Into<Body>,
    ) -> Response {
        let mut request = Request::builder().method(method).uri(uri);
        if let Some(content_type) = content_type {
            request = request.header(header::CONTENT_TYPE, content_type);
        }
        app.oneshot(request.body(body.into()).unwrap())
            .await
            .unwrap()
    }

    async fn json(response: Response) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn registration_enforces_media_type_validation_and_idempotency() {
        let control = Arc::new(TestControl::default());

        let unsupported = call(
            app(control.clone()),
            Method::POST,
            "/api/v1/modules",
            Some("application/octet-stream"),
            "wasm",
        )
        .await;
        assert_eq!(unsupported.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        let empty = call(
            app(control.clone()),
            Method::POST,
            "/api/v1/modules",
            Some("application/wasm"),
            Body::empty(),
        )
        .await;
        assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

        let invalid = call(
            app(control.clone()),
            Method::POST,
            "/api/v1/modules",
            Some("application/wasm; charset=binary"),
            "invalid",
        )
        .await;
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(json(invalid).await["error"]["code"], "module_invalid");

        let created = call(
            app(control.clone()),
            Method::POST,
            "/api/v1/modules",
            Some("application/wasm"),
            "wasm",
        )
        .await;
        let id = TestControl::module_id(b"wasm");
        assert_eq!(created.status(), StatusCode::CREATED);
        assert_eq!(
            created.headers()[header::LOCATION],
            format!("/api/v1/modules/{id}")
        );
        let created_body = json(created).await;
        assert_eq!(created_body["id"], id);
        assert_eq!(created_body["size_bytes"], 4);

        let duplicate = call(
            app(control),
            Method::POST,
            "/api/v1/modules",
            Some("application/wasm"),
            "wasm",
        )
        .await;
        assert_eq!(duplicate.status(), StatusCode::OK);
        assert!(!duplicate.headers().contains_key(header::LOCATION));
    }

    #[tokio::test]
    async fn module_routes_cover_list_get_run_and_idempotent_delete() {
        let control = Arc::new(TestControl::default());
        let registered = call(
            app(control.clone()),
            Method::POST,
            "/api/v1/modules",
            Some("application/wasm"),
            "wasm",
        )
        .await;
        let id = json(registered).await["id"].as_str().unwrap().to_string();

        let listed = call(
            app(control.clone()),
            Method::GET,
            "/api/v1/modules?limit=1",
            None,
            Body::empty(),
        )
        .await;
        assert_eq!(listed.status(), StatusCode::OK);
        assert_eq!(json(listed).await["items"][0]["id"], id);

        let inspected = call(
            app(control.clone()),
            Method::GET,
            &format!("/api/v1/modules/{id}"),
            None,
            Body::empty(),
        )
        .await;
        assert_eq!(inspected.status(), StatusCode::OK);

        let run = call(
            app(control.clone()),
            Method::POST,
            &format!("/api/v1/modules/{id}/runs"),
            None,
            Body::empty(),
        )
        .await;
        assert_eq!(run.status(), StatusCode::NO_CONTENT);

        for _ in 0..2 {
            let deleted = call(
                app(control.clone()),
                Method::DELETE,
                &format!("/api/v1/modules/{id}"),
                None,
                Body::empty(),
            )
            .await;
            assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
        }

        let missing = call(
            app(control),
            Method::GET,
            &format!("/api/v1/modules/{id}"),
            None,
            Body::empty(),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        assert_eq!(json(missing).await["error"]["message"], "module not found");

        let execution_failed = call(
            app(Arc::new(TestControl::default())),
            Method::POST,
            &format!("/api/v1/modules/{}/runs", "f".repeat(64)),
            None,
            Body::empty(),
        )
        .await;
        assert_eq!(execution_failed.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            json(execution_failed).await["error"]["code"],
            "module_execution_failed"
        );
    }

    #[tokio::test]
    async fn collection_queries_and_binary_keys_are_strict_and_binary_safe() {
        let control = Arc::new(TestControl::default());

        for uri in [
            "/api/v1/modules?limit=0",
            "/api/v1/modules?limit=101",
            "/api/v1/modules?unknown=true",
            "/api/v1/modules?cursor=not-a-module-cursor",
            "/api/v1/keys?prefix=%25%25%25",
            "/api/v1/keys/%FF",
        ] {
            let response = call(app(control.clone()), Method::GET, uri, None, Body::empty()).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        }

        let prefix = URL_SAFE_NO_PAD.encode(b"app:");
        let keys = call(
            app(control.clone()),
            Method::GET,
            &format!("/api/v1/keys?prefix={prefix}&limit=1"),
            None,
            Body::empty(),
        )
        .await;
        assert_eq!(keys.status(), StatusCode::OK);
        let keys = json(keys).await;
        assert_eq!(keys["items"][0], URL_SAFE_NO_PAD.encode(b"app:a"));
        assert_eq!(keys["next_cursor"], URL_SAFE_NO_PAD.encode(b"app:a"));

        let binary_key = URL_SAFE_NO_PAD.encode([0, 255, b'k']);
        let value = call(
            app(control),
            Method::GET,
            &format!("/api/v1/keys/{binary_key}"),
            None,
            Body::empty(),
        )
        .await;
        assert_eq!(value.status(), StatusCode::OK);
        assert_eq!(
            value.headers()[header::CONTENT_TYPE],
            "application/octet-stream"
        );
        assert_eq!(value.headers()["x-numax-key"], binary_key);
        assert_eq!(
            value.into_body().collect().await.unwrap().to_bytes(),
            Bytes::from_static(&[0, 1, 255])
        );
    }

    #[tokio::test]
    async fn peer_pagination_probes_and_fallbacks_follow_the_json_contract() {
        let control = Arc::new(TestControl::default());

        let peers = call(
            app(control.clone()),
            Method::GET,
            "/api/v1/peers?limit=1",
            None,
            Body::empty(),
        )
        .await;
        assert_eq!(peers.status(), StatusCode::OK);
        let peers = json(peers).await;
        assert_eq!(peers["items"][0]["node_id"], "node-a");
        assert_eq!(
            peers["next_cursor"],
            URL_SAFE_NO_PAD.encode(b"127.0.0.1:9001")
        );

        let health = call(
            app(control.clone()),
            Method::GET,
            "/api/v1/health",
            None,
            Body::empty(),
        )
        .await;
        assert_eq!(health.status(), StatusCode::OK);
        assert_eq!(json(health).await["status"], "healthy");

        let not_ready = call(
            app(control.clone()),
            Method::GET,
            "/api/v1/ready",
            None,
            Body::empty(),
        )
        .await;
        assert_eq!(not_ready.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(not_ready.headers()[header::RETRY_AFTER], "1");
        control.ready.store(true, Ordering::SeqCst);
        let ready = call(
            app(control.clone()),
            Method::GET,
            "/api/v1/ready",
            None,
            Body::empty(),
        )
        .await;
        assert_eq!(ready.status(), StatusCode::OK);

        let missing = call(
            app(control.clone()),
            Method::GET,
            "/api/v1/missing",
            None,
            Body::empty(),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        assert_eq!(json(missing).await["error"]["code"], "not_found");

        let wrong_method = call(
            app(control),
            Method::PATCH,
            "/api/v1/health",
            None,
            Body::empty(),
        )
        .await;
        assert_eq!(wrong_method.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(json(wrong_method).await["error"]["code"], "invalid_request");
    }

    #[tokio::test]
    async fn key_pages_are_trimmed_to_the_response_body_limit() {
        let keys = (0..100)
            .map(|index| vec![index; MAX_KEY_SIZE])
            .collect::<Vec<_>>();

        let response = bounded_key_page(keys, None);
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(bytes.len() <= MAX_RESPONSE_BODY_SIZE);

        let page: Value = serde_json::from_slice(&bytes).unwrap();
        let items = page["items"].as_array().unwrap();
        assert!(!items.is_empty());
        assert!(items.len() < 100);
        assert_eq!(page["next_cursor"], *items.last().unwrap());
    }
}
