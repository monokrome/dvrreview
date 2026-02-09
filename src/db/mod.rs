pub mod migrations;
pub mod models;
pub mod schema;

use std::error::Error;
use diesel::ConnectionError;
use diesel::ConnectionResult;
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::pooled_connection::{AsyncDieselConnectionManager, ManagerConfig};
use diesel_async::AsyncPgConnection;
use futures_util::future::BoxFuture;
use futures_util::FutureExt;
use tokio_postgres::NoTls;
use tokio_postgres_rustls::MakeRustlsConnect;

pub type DbPool = Pool<AsyncPgConnection>;

pub(crate) fn make_tls_connector() -> MakeRustlsConnect {
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    MakeRustlsConnect::new(config)
}

pub(crate) fn should_use_tls(url: &str) -> bool {
    if url.contains("sslmode=disable") || url.contains("sslmode=allow") {
        return false;
    }
    !url.contains("localhost") && !url.contains("127.0.0.1") && !url.contains("10.200.0.")
}

/// Rewrite sslmode to a value the Rust postgres crates accept (`require` or `disable`).
/// Certificate verification is handled by the rustls connector, not the sslmode parameter.
pub(crate) fn normalize_sslmode(url: &str, use_tls: bool) -> String {
    let target = if use_tls { "sslmode=require" } else { "sslmode=disable" };
    if let Some(start) = url.find("sslmode=") {
        let end = url[start..].find('&').map(|i| start + i).unwrap_or(url.len());
        format!("{}{}{}", &url[..start], target, &url[end..])
    } else {
        let sep = if url.contains('?') { "&" } else { "?" };
        format!("{}{}{}", url, sep, target)
    }
}

pub fn create_pool(database_url: &str) -> DbPool {
    let mut config = ManagerConfig::default();
    config.custom_setup = Box::new(establish_connection);

    let mgr = AsyncDieselConnectionManager::<AsyncPgConnection>::new_with_config(
        database_url,
        config,
    );

    Pool::builder(mgr)
        .build()
        .expect("Failed to create database pool")
}

fn establish_connection(url: &str) -> BoxFuture<'_, ConnectionResult<AsyncPgConnection>> {
    let use_tls = should_use_tls(url);
    let url = normalize_sslmode(url, use_tls);
    async move {
        let client = if use_tls {
            let tls = make_tls_connector();
            let (client, conn) = tokio_postgres::connect(&url, tls)
                .await
                .map_err(|e| {
                    let msg = if let Some(source) = e.source() {
                        format!("{}: {}", e, source)
                    } else {
                        format!("{:?}", e)
                    };
                    ConnectionError::BadConnection(msg)
                })?;

            tokio::spawn(async move {
                if let Err(e) = conn.await {
                    if let Some(source) = e.source() {
                        tracing::error!("Database connection error: {} ({})", e, source);
                    } else {
                        tracing::error!("Database connection error: {:?}", e);
                    }
                }
            });
            client
        } else {
            let (client, conn) = tokio_postgres::connect(&url, NoTls)
                .await
                .map_err(|e| {
                    let msg = if let Some(source) = e.source() {
                        format!("{}: {}", e, source)
                    } else {
                        format!("{:?}", e)
                    };
                    ConnectionError::BadConnection(msg)
                })?;

            tokio::spawn(async move {
                if let Err(e) = conn.await {
                    if let Some(source) = e.source() {
                        tracing::error!("Database connection error: {} ({})", e, source);
                    } else {
                        tracing::error!("Database connection error: {:?}", e);
                    }
                }
            });
            client
        };

        AsyncPgConnection::try_from(client)
            .await
            .map_err(|e| ConnectionError::BadConnection(e.to_string()))
    }
    .boxed()
}
