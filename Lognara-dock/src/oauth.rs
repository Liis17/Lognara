//! OAuth authorization code flow for ChatGPT CIMD clients.
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use axum::{
    Form, Json, Router,
    extract::{Query, State, rejection::FormRejection},
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    middleware,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::lookup_host;
use tower_http::set_header::SetResponseHeaderLayer;
use url::Url;
use uuid::Uuid;

use crate::{
    api::{ApiError, AppState},
    mcp_access,
    session::{self, SESSION_COOKIE},
    store::{self, PendingAuthorization, RefreshOutcome},
};

const SCOPE: &str = "lognara.read";
const CHATGPT_HOST: &str = "chatgpt.com";
const CIMD_LIMIT: usize = 256 * 1024;

pub fn router(app: AppState) -> Router {
    if !app.config.mcp_enabled {
        return Router::new()
            .route("/.well-known/oauth-protected-resource", get(disabled))
            .route("/.well-known/oauth-protected-resource/mcp", get(disabled))
            .route("/.well-known/oauth-authorization-server", get(disabled))
            .route("/oauth/authorize", get(disabled))
            .route("/oauth/token", post(disabled));
    }
    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server_metadata),
        )
        .route("/oauth/authorize", get(authorize))
        .route("/oauth/token", post(token))
        .layer(SetResponseHeaderLayer::if_not_present(
            CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .layer(middleware::from_fn(allowed_methods));
}

async fn disabled() -> StatusCode {
    StatusCode::NOT_FOUND
}

async fn allowed_methods(request: axum::extract::Request, next: middleware::Next) -> Response {
    if request.method() != axum::http::Method::GET && request.method() != axum::http::Method::POST {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    next.run(request).await
}

fn resource_uri(app: &AppState) -> String {
    app.config
        .public_url
        .join("mcp")
        .expect("valid MCP URL")
        .as_str()
        .to_owned()
}

async fn protected_resource_metadata(State(app): State<AppState>) -> Json<Value> {
    Json(json!({
        "resource": resource_uri(&app),
        "authorization_servers": [app.config.public_origin],
        "scopes_supported": [SCOPE],
        "bearer_methods_supported": ["header"],
    }))
}

async fn authorization_server_metadata(State(app): State<AppState>) -> Json<Value> {
    let origin = &app.config.public_origin;
    Json(json!({
        "issuer": origin,
        "authorization_endpoint": format!("{origin}/oauth/authorize"),
        "token_endpoint": format!("{origin}/oauth/token"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": [SCOPE],
        "code_challenge_methods_supported": ["S256"],
        "client_id_metadata_document_supported": true,
        "authorization_response_iss_parameter_supported": true,
    }))
}

#[derive(Debug, Deserialize, Serialize)]
struct AuthorizationRequest {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    state: String,
    code_challenge: String,
    code_challenge_method: String,
    scope: Option<String>,
    resource: Option<String>,
}

async fn authorize(
    State(app): State<AppState>,
    Query(request): Query<AuthorizationRequest>,
    headers: axum::http::HeaderMap,
) -> Result<Response, ApiError> {
    let username = match session::read_cookie(&headers, SESSION_COOKIE) {
        Some(token) => match app.sessions.lookup(token).await? {
            Some((username, _hash)) if app.htpasswd.contains(&username) => Some(username),
            Some((_, hash)) => {
                app.sessions.revoke(hash).await?;
                None
            }
            None => None,
        },
        None => None,
    };
    let Some(username) = username else {
        let query = serde_urlencoded::to_string(&request)
            .map_err(|_| ApiError::BadRequest("invalid_request"))?;
        let mut login = Url::parse(&format!("{}/login", app.config.public_origin))
            .map_err(|_| ApiError::Internal)?;
        login
            .query_pairs_mut()
            .append_pair("return_to", &format!("/oauth/authorize?{query}"));
        return Ok(Redirect::temporary(login.as_str()).into_response());
    };

    let metadata = fetch_client_metadata(&request.client_id)
        .await
        .map_err(|_| ApiError::BadRequest("invalid_client"))?;
    if !metadata
        .redirect_uris
        .iter()
        .any(|uri| uri == &request.redirect_uri && valid_chatgpt_redirect(uri))
    {
        return Err(ApiError::BadRequest("invalid_redirect_uri"));
    }
    let resource = resource_uri(&app);
    if request.response_type != "code"
        || request.state.is_empty()
        || request.state.len() > 512
        || request.state.chars().any(char::is_control)
        || request.code_challenge_method != "S256"
        || !valid_challenge(&request.code_challenge)
        || request.scope.as_deref().is_some_and(|scope| scope != SCOPE)
        || request.resource.as_deref() != Some(resource.as_str())
    {
        return Ok(authorization_error(
            &app.config.public_origin,
            &request.redirect_uri,
            &request.state,
            "invalid_request",
        ));
    }
    let pending = PendingAuthorization {
        id: Uuid::new_v4().to_string(),
        username,
        client_id: request.client_id,
        client_name: metadata.client_name,
        redirect_uri: request.redirect_uri,
        state: request.state,
        scope: SCOPE.into(),
        resource,
        code_challenge: request.code_challenge,
        expires_at: store::now_ms() + 300_000,
    };
    app.store.create_oauth_pending(pending.clone()).await?;
    let mut consent = Url::parse(&format!("{}/oauth/consent", app.config.public_origin))
        .map_err(|_| ApiError::Internal)?;
    consent
        .query_pairs_mut()
        .append_pair("request", &pending.id);
    Ok(Redirect::temporary(consent.as_str()).into_response())
}

#[derive(Debug, Deserialize)]
struct ClientMetadata {
    client_id: String,
    client_name: String,
    redirect_uris: Vec<String>,
    response_types: Vec<String>,
    grant_types: Vec<String>,
    token_endpoint_auth_methods_supported: Option<Vec<String>>,
    token_endpoint_auth_method: Option<String>,
}

async fn fetch_client_metadata(client_id: &str) -> Result<ClientMetadata, ()> {
    let url = valid_chatgpt_client_id(client_id)?;
    let addresses = lookup_host((CHATGPT_HOST, 443))
        .await
        .map_err(|_| ())?
        .filter(|address| is_public_ip(address.ip()))
        .map(|address| SocketAddr::new(address.ip(), 443))
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(());
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(5))
        .resolve_to_addrs(CHATGPT_HOST, &addresses)
        .build()
        .map_err(|_| ())?;
    let mut response = client
        .get(url.clone())
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|_| ())?;
    if !response.status().is_success()
        || response.url() != &url
        || response
            .content_length()
            .is_some_and(|length| length > CIMD_LIMIT as u64)
    {
        return Err(());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if bytes.len().saturating_add(chunk.len()) > CIMD_LIMIT {
            return Err(());
        }
        bytes.extend_from_slice(&chunk);
    }
    let metadata: ClientMetadata = serde_json::from_slice(&bytes).map_err(|_| ())?;
    validate_client_metadata(metadata, client_id)
}

fn validate_client_metadata(
    metadata: ClientMetadata,
    client_id: &str,
) -> Result<ClientMetadata, ()> {
    if metadata.client_id != client_id
        || metadata.client_name.trim().is_empty()
        || !metadata.response_types.iter().any(|value| value == "code")
        || !metadata
            .grant_types
            .iter()
            .any(|value| value == "authorization_code")
        || !(metadata
            .token_endpoint_auth_methods_supported
            .as_ref()
            .is_some_and(|values| values.iter().any(|value| value == "none"))
            || metadata.token_endpoint_auth_method.as_deref() == Some("none"))
        || metadata.redirect_uris.is_empty()
        || metadata
            .redirect_uris
            .iter()
            .any(|uri| !valid_chatgpt_redirect(uri))
    {
        return Err(());
    }
    Ok(metadata)
}

fn valid_chatgpt_client_id(value: &str) -> Result<Url, ()> {
    let url = Url::parse(value).map_err(|_| ())?;
    if url.as_str() != value
        || url.scheme() != "https"
        || url.host_str() != Some(CHATGPT_HOST)
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    let segments = url.path_segments().ok_or(())?.collect::<Vec<_>>();
    let valid = match segments.as_slice() {
        ["oauth", "client.json"] => true,
        ["oauth", callback, "client.json"] => {
            !callback.is_empty()
                && callback
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        }
        _ => false,
    };
    valid.then_some(url).ok_or(())
}

fn valid_chatgpt_redirect(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    if url.as_str() != value
        || url.scheme() != "https"
        || url.host_str() != Some(CHATGPT_HOST)
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let segments = url
        .path_segments()
        .map(|segments| segments.collect::<Vec<_>>())
        .unwrap_or_default();
    matches!(segments.as_slice(), ["connector_platform_oauth_redirect"])
        || matches!(segments.as_slice(), ["connector", "oauth", callback]
            if !callback.is_empty() && callback.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte,b'-'|b'_')))
}

fn valid_challenge(challenge: &str) -> bool {
    URL_SAFE_NO_PAD
        .decode(challenge)
        .is_ok_and(|bytes| bytes.len() == 32)
}

fn authorization_error(issuer: &str, redirect_uri: &str, state: &str, error: &str) -> Response {
    let mut redirect = Url::parse(redirect_uri).expect("redirect URI was validated");
    {
        let mut query = redirect.query_pairs_mut();
        query.append_pair("error", error);
        if !state.is_empty() && state.len() <= 512 && !state.chars().any(char::is_control) {
            query.append_pair("state", state);
        }
        query.append_pair("iss", issuer);
    }
    Redirect::temporary(redirect.as_str()).into_response()
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_ipv4(ip),
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return public_ipv4(mapped);
            }
            let segments = ip.segments();
            (segments[0] & 0xe000) == 0x2000
                && !(segments[0] == 0x2001 && segments[1] <= 0x01ff)
                && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
                && segments[0] != 0x2002
                && segments[0] != 0x3fff
                && !(segments[0] == 0x0064 && segments[1] == 0xff9b)
        }
    }
}

fn public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !ip.is_private()
        && !ip.is_loopback()
        && !ip.is_link_local()
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && ip != Ipv4Addr::BROADCAST
        && a != 0
        && !(a == 100 && (64..=127).contains(&b))
        && !(a == 192 && b == 0)
        && !(a == 192 && b == 88 && c == 99)
        && !(a == 198 && (b == 18 || b == 19))
        && !(a == 198 && b == 51 && c == 100)
        && !(a == 203 && b == 0 && c == 113)
        && a < 240
}

#[derive(Debug, Deserialize)]
struct TokenRequest {
    grant_type: String,
    client_id: String,
    resource: String,
    code: Option<String>,
    redirect_uri: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
}

async fn token(
    State(app): State<AppState>,
    form: Result<Form<TokenRequest>, FormRejection>,
) -> Response {
    let Ok(Form(request)) = form else {
        return token_error("invalid_request");
    };
    if valid_chatgpt_client_id(&request.client_id).is_err()
        || request.resource != resource_uri(&app)
    {
        return token_error("invalid_client");
    }
    match request.grant_type.as_str() {
        "authorization_code" => exchange_code(&app, request).await,
        "refresh_token" => refresh_token(&app, request).await,
        _ => token_error("unsupported_grant_type"),
    }
}

async fn exchange_code(app: &AppState, request: TokenRequest) -> Response {
    let (Some(code), Some(redirect_uri), Some(verifier)) =
        (request.code, request.redirect_uri, request.code_verifier)
    else {
        return token_error("invalid_request");
    };
    if verifier.len() < 43
        || verifier.len() > 128
        || !verifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
    {
        return token_error("invalid_grant");
    }
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let access_token = mcp_access::random_token();
    let refresh_token = mcp_access::random_token();
    let code_hash = Sha256::digest(code.as_bytes()).into();
    let access_hash = Sha256::digest(access_token.as_bytes()).into();
    let refresh_hash = Sha256::digest(refresh_token.as_bytes()).into();
    let issued = match app
        .store
        .redeem_oauth_code(
            code_hash,
            request.client_id,
            redirect_uri,
            request.resource,
            challenge,
            access_hash,
            refresh_hash,
        )
        .await
    {
        Ok(Some(issued)) => issued,
        Ok(None) => return token_error("invalid_grant"),
        Err(error) => {
            tracing::error!(%error, "OAuth code exchange failed");
            return token_error("server_error");
        }
    };
    if !app.htpasswd.contains(&issued.username) {
        let _ = app.store.revoke_user_mcp_credentials(issued.username).await;
        return token_error("invalid_grant");
    }
    token_success(access_token, refresh_token, issued.scope)
}

async fn refresh_token(app: &AppState, request: TokenRequest) -> Response {
    let Some(refresh) = request.refresh_token else {
        return token_error("invalid_request");
    };
    let access_token = mcp_access::random_token();
    let next_refresh = mcp_access::random_token();
    let refresh_hash = Sha256::digest(refresh.as_bytes()).into();
    let access_hash = Sha256::digest(access_token.as_bytes()).into();
    let next_refresh_hash = Sha256::digest(next_refresh.as_bytes()).into();
    let (outcome, issued) = match app
        .store
        .rotate_oauth_refresh(
            refresh_hash,
            request.client_id,
            request.resource,
            access_hash,
            next_refresh_hash,
        )
        .await
    {
        Ok(result) => result,
        Err(error) => {
            tracing::error!(%error, "OAuth refresh failed");
            return token_error("server_error");
        }
    };
    match (outcome, issued) {
        (RefreshOutcome::Issued, Some(issued)) if app.htpasswd.contains(&issued.username) => {
            token_success(access_token, next_refresh, issued.scope)
        }
        (RefreshOutcome::Issued, Some(issued)) => {
            let _ = app.store.revoke_user_mcp_credentials(issued.username).await;
            token_error("invalid_grant")
        }
        (RefreshOutcome::Replay, _) | (RefreshOutcome::Invalid, _) => token_error("invalid_grant"),
        _ => token_error("invalid_grant"),
    }
}

fn token_success(access_token: String, refresh_token: String, scope: String) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "access_token": access_token,
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": refresh_token,
            "scope": scope,
        })),
    )
        .into_response()
}

fn token_error(error: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cimd_urls_are_confined_to_the_chatgpt_client_origin() {
        assert!(valid_chatgpt_client_id("https://chatgpt.com/oauth/client.json").is_ok());
        assert!(
            valid_chatgpt_client_id("https://chatgpt.com/oauth/callback_1/client.json").is_ok()
        );
        for value in [
            "https://chatgpt.com.evil.example/oauth/client.json",
            "http://chatgpt.com/oauth/client.json",
            "https://chatgpt.com/oauth/client.json?url=http://127.0.0.1",
            "https://chatgpt.com:443/oauth/client.json",
            "https://chatgpt.com/oauth/a/b/client.json",
            "https://127.0.0.1/oauth/client.json",
        ] {
            assert!(valid_chatgpt_client_id(value).is_err(), "{value}");
        }
    }

    #[test]
    fn accepts_the_current_chatgpt_cimd_document_shape_and_only_public_auth() {
        let client_id = "https://chatgpt.com/oauth/client.json";
        let metadata: ClientMetadata = serde_json::from_str(&format!(
            r#"{{
                "client_id":"{client_id}",
                "client_name":"ChatGPT",
                "redirect_uris":["https://chatgpt.com/connector_platform_oauth_redirect"],
                "response_types":["code"],
                "grant_types":["authorization_code","refresh_token"],
                "token_endpoint_auth_method":"private_key_jwt",
                "token_endpoint_auth_methods_supported":["none","private_key_jwt"]
            }}"#
        ))
        .unwrap();
        assert!(validate_client_metadata(metadata, client_id).is_ok());

        let unsupported: ClientMetadata = serde_json::from_str(&format!(
            r#"{{
                "client_id":"{client_id}",
                "client_name":"ChatGPT",
                "redirect_uris":["https://chatgpt.com/connector_platform_oauth_redirect"],
                "response_types":["code"],
                "grant_types":["authorization_code"],
                "token_endpoint_auth_methods_supported":["private_key_jwt"]
            }}"#
        ))
        .unwrap();
        assert!(validate_client_metadata(unsupported, client_id).is_err());
    }

    #[test]
    fn redirects_and_metadata_fetch_addresses_reject_untrusted_destinations() {
        assert!(valid_chatgpt_redirect(
            "https://chatgpt.com/connector_platform_oauth_redirect"
        ));
        assert!(valid_chatgpt_redirect(
            "https://chatgpt.com/connector/oauth/callback_1"
        ));
        for value in [
            "https://evil.example/connector_platform_oauth_redirect",
            "http://chatgpt.com/connector_platform_oauth_redirect",
            "https://chatgpt.com/connector_platform_oauth_redirect?next=https://evil.example",
            "https://chatgpt.com/connector/oauth/callback/extra",
        ] {
            assert!(!valid_chatgpt_redirect(value), "{value}");
        }
        for value in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.1.1",
            "192.0.2.1",
            "192.88.99.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "2001:2::1",
            "2002:0808:0808::1",
            "64:ff9b::808:808",
            "3fff::1",
        ] {
            assert!(!is_public_ip(value.parse().unwrap()), "{value}");
        }
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }
}
