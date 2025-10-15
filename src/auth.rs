use actix_web::{
    dev::{Service, ServiceRequest, ServiceResponse, Transform},
    FromRequest, HttpMessage, HttpResponse,
};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose, Engine as _};
use futures::{executor::block_on, future::LocalBoxFuture};
use isahc::ReadResponseExt;
use lazy_static::lazy_static;
use log::{log, Level};
use openssl::{
    bn::BigNum,
    hash::MessageDigest,
    pkey::{PKey, Public},
    rsa::Rsa,
    sign::Verifier,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::future::{ready, Ready};
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::Mutex;

lazy_static! {
    static ref JWT_CACHE: Arc<Mutex<HashMap<String, PKey<Public>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    pub static ref SSO_ENABLED: bool = env::var("SSO_ENABLED")
        .map(|x| x.parse::<bool>().unwrap_or(true))
        .unwrap_or(true);
    pub static ref SSO_AUTHORITY: String = env::var("SSO_AUTHORITY").unwrap_or_else(|_| "".into());
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenHeader {
    alg: String,
    kid: String,
    typ: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    exp: u32,
    iat: Option<u32>,
    auth_time: Option<u32>,
    jti: Option<String>,
    iss: Option<String>,
    aud: Option<String>,
    sub: String,
    typ: Option<String>,
    azp: Option<String>,
    nonce: Option<String>,
    session_state: Option<String>,
    scope: Option<String>,
    sid: Option<String>,
    email_verified: Option<bool>,
    pub name: Option<String>,
    pub groups: Option<Vec<String>>,
    pub preferred_username: String,
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    pub email: Option<String>,
}

impl FromRequest for User {
    type Error = actix_web::error::Error;
    type Future = LocalBoxFuture<'static, Result<Self, Self::Error>>;

    fn from_request(
        req: &actix_web::HttpRequest,
        _payload: &mut actix_web::dev::Payload,
    ) -> Self::Future {
        let unauthorized = || {
            Box::pin(async {
                <Result<Self, Self::Error>>::Err(actix_web::error::ErrorUnauthorized(""))
            })
        };

        let token = req
            .headers()
            .get("Authorization")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .map(|s| s.to_string());

        let h = match token {
            Some(h) => h,
            None => return unauthorized(),
        };

        let (head, head_64, user, user_64, sig) = match get_token_pieces(h) {
            Ok(vals) => vals,
            Err(_) => return unauthorized(),
        };

        if verify_token(&head, &head_64, &user, &user_64, &sig) {
            Box::pin(async { Ok(user) })
        } else {
            unauthorized()
        }
    }
}

fn get_token_pieces(token: String) -> Result<(TokenHeader, String, User, String, Vec<u8>)> {
    let mut it = token.split('.');
    let token_header_base64 = it.next().ok_or(anyhow!("!header"))?;
    let token_header = general_purpose::URL_SAFE_NO_PAD.decode(token_header_base64)?;
    let token_header: TokenHeader = serde_json::from_slice(&token_header)?;
    let token_payload_base64 = it.next().ok_or(anyhow!("!body"))?;
    let token_payload = general_purpose::URL_SAFE_NO_PAD.decode(token_payload_base64)?;
    let token_payload: User = serde_json::from_slice(&token_payload)?;
    let token_signature = it.next().ok_or(anyhow!("signature"))?;
    let token_signature = general_purpose::URL_SAFE_NO_PAD.decode(token_signature)?;
    Ok((
        token_header,
        token_header_base64.to_owned(),
        token_payload,
        token_payload_base64.to_owned(),
        token_signature,
    ))
}

#[allow(unused_must_use)]
fn verify_token(
    header: &TokenHeader,
    header_64: &String,
    payload: &User,
    payload_64: &String,
    key: &[u8],
) -> bool {
    if payload.exp < (chrono::Utc::now().timestamp() as u32) {
        return false;
    }
    if header.alg != "RS256" {
        return false;
    }

    let data_cache = JWT_CACHE.clone();
    let mut cache = block_on(data_cache.lock());
    let pkey = match cache.get(header.kid.as_str()) {
        Some(x) => Some(x),
        None => {
            if let Err(e) = update_cache(&mut cache) {
                log!(Level::Error, "Failed to update JWKS cache: {e:?}");
                None
            } else {
                cache.get(header.kid.as_str())
            }
        }
    };

    let pkey = match pkey {
        Some(p) => p,
        None => return false,
    };

    let mut verifier = Verifier::new(MessageDigest::sha256(), pkey).unwrap();
    verifier.update(header_64.as_bytes());
    verifier.update(b".");
    verifier.update(payload_64.as_bytes());
    verifier.verify(key).unwrap_or(false)
}

#[derive(Serialize, Deserialize, Debug)]
struct CertKey {
    kid: String,
    kty: String,
    alg: String,
    r#use: String,
    n: String,
    e: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct CertData {
    keys: Vec<CertKey>,
}

fn update_cache(cache: &mut HashMap<String, PKey<Public>>) -> Result<()> {
    let authority = SSO_AUTHORITY.clone();
    if authority.is_empty() {
        return Err(anyhow!("SSO_AUTHORITY not set"));
    }
    let url = format!(
        "{}/protocol/openid-connect/certs",
        authority.trim_end_matches('/')
    );

    let cert_data: CertData = isahc::get(&url).unwrap().json().unwrap();

    for key in cert_data.keys {
        if cache.contains_key(key.kid.as_str()) {
            continue;
        }
        let n: Vec<String> = general_purpose::URL_SAFE_NO_PAD
            .decode(key.n.as_bytes())?
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect();
        let e: Vec<String> = general_purpose::URL_SAFE_NO_PAD
            .decode(key.e.as_bytes())?
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect();
        let n = BigNum::from_hex_str(&n.join(""))?;
        let e = BigNum::from_hex_str(&e.join(""))?;
        let rsa = Rsa::from_public_components(n, e)?;
        cache.insert(key.kid, PKey::from_rsa(rsa)?);
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct CSHAuth {
    enabled: bool,
}

impl CSHAuth {
    pub fn enabled() -> Self {
        Self {
            enabled: *SSO_ENABLED,
        }
    }

    pub fn disabled() -> Self {
        Self { enabled: false }
    }
}

pub struct CSHAuthService<S> {
    service: S,
    enabled: bool,
}

impl<S> Transform<S, ServiceRequest> for CSHAuth
where
    S: Service<
        ServiceRequest,
        Response = ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    S::Future: 'static,
{
    type Response = ServiceResponse<actix_web::body::BoxBody>;
    type Error = actix_web::Error;
    type Transform = CSHAuthService<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(CSHAuthService {
            service,
            enabled: self.enabled,
        }))
    }
}

impl<S> Service<ServiceRequest> for CSHAuthService<S>
where
    S: Service<
        ServiceRequest,
        Response = ServiceResponse<actix_web::body::BoxBody>,
        Error = actix_web::Error,
    >,
    S::Future: 'static,
{
    type Response = ServiceResponse<actix_web::body::BoxBody>;
    type Error = actix_web::Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&self, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(ctx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        if self.enabled {
            let unauthorized = |req: ServiceRequest| -> <Self as Service<ServiceRequest>>::Future {
                Box::pin(async { Ok(req.into_response(HttpResponse::Unauthorized().finish())) })
            };

            let token = match req.headers().get("Authorization").map(|x| x.to_str()) {
                Some(Ok(x)) => x.trim_start_matches("Bearer ").to_string(),
                _ => return unauthorized(req),
            };

            let (
                token_header,
                token_header_base64,
                token_payload,
                token_payload_base64,
                token_signature,
            ) = match get_token_pieces(token) {
                Ok(x) => x,
                Err(e) => {
                    log!(Level::Debug, "Token is formatted incorrectly: {e}");
                    return unauthorized(req);
                }
            };

            let verified = verify_token(
                &token_header,
                &token_header_base64,
                &token_payload,
                &token_payload_base64,
                &token_signature,
            );

            if verified {
                req.extensions_mut().insert(token_payload.clone());
            } else {
                return unauthorized(req);
            }
        }

        let fut = self.service.call(req);
        Box::pin(async move { Ok(fut.await?) })
    }
}
