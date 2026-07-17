mod common;
use common::*;

#[tokio::test]
async fn recording_transport_captures_send() {
    let env = test_state().await;
    let cfg = vigil::notify::SmtpConfig { host: "h".into(), port: 25, security: "none".into() };
    let msg = vigil::notify::EmailMsg {
        to: vec!["a@b".into()],
        from: "f@b".into(),
        subject: "s".into(),
        body_text: "t".into(),
        body_html: None,
    };
    env.state.transport.send(&cfg, &msg).await.unwrap();
    assert_eq!(env.sent.lock().unwrap().len(), 1);
}
