#[tokio::test]
async fn healthz_returns_ok() {
    let app = vigil::app::router_health_only();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
    let body = reqwest::get(format!("http://{addr}/healthz")).await.unwrap().text().await.unwrap();
    assert_eq!(body, "ok");
}
