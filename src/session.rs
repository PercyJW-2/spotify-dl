use anyhow::Result;
use librespot::core::cache::Cache;
use librespot::core::config::SessionConfig;
use librespot::core::session::Session;
use librespot::discovery::Credentials;

pub async fn create_session() -> Result<Session> {
    let credentials_store = dirs::home_dir().map(|p| p.join(".spotify-dl"));
    let cache = Cache::new(credentials_store, None, None, None)?;

    let session_config = SessionConfig::default();
    
    let credentials = cache
        .credentials()
        .ok_or_else(|| {
            librespot_oauth::get_access_token(
                &session_config.client_id,
                "http://127.0.0.1:8898/login",
                vec!["streaming"]
            )
                .map(|t| Credentials::with_access_token(t.access_token))
                .unwrap()
        }).unwrap_or_else(|c| c);

    let session = Session::new(session_config, Some(cache));
    session.connect(credentials, true).await?;
    Ok(session)
}
