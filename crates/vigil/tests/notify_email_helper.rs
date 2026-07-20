mod common;
use common::test_state;
use vigil::notify::dispatch::send_email_via_channel;

#[tokio::test]
async fn helper_parses_config_and_sends_email() {
    let env = test_state().await;
    let cfg = r#"{"host":"smtp.example.com","port":587,"security":"starttls","from":"a@b.com","to":["x@y.com","z@y.com"]}"#;

    send_email_via_channel(env.state.transport.as_ref(), cfg, "Subj", "Body", None)
        .await
        .unwrap();

    let sent = env.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    let (smtp, msg) = &sent[0];
    assert_eq!(smtp.host, "smtp.example.com");
    assert_eq!(smtp.port, 587);
    assert_eq!(msg.from, "a@b.com");
    assert_eq!(msg.to, vec!["x@y.com".to_string(), "z@y.com".to_string()]);
    assert_eq!(msg.subject, "Subj");
    assert_eq!(msg.body_text, "Body");
}
