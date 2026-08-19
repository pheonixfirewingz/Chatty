use chatty_protocol::{BrokerMonitor, Response, decode, encode};

#[test]
fn monitoring_response_round_trips_over_binary_protocol() {
    let response = Response::BrokerMonitor(BrokerMonitor {
        uptime_seconds: 42,
        cpu_percent: 1.5,
        memory_used_mb: 38,
        memory_limit_mb: Some(512),
        active_connections: 3,
        adapter_status: chatty_protocol::AdapterStatus::Offline,
        adapter_model_count: 0,
        adapter_latency_ms: Some(2000),
        recent_errors: vec!["adapter unavailable".into()],
    });
    let decoded: Response = decode(&encode(&response).unwrap()).unwrap();
    match decoded {
        Response::BrokerMonitor(monitor) => {
            assert_eq!(monitor.uptime_seconds, 42);
            assert_eq!(monitor.active_connections, 3);
            assert_eq!(monitor.recent_errors.len(), 1);
            assert_eq!(
                monitor.adapter_status,
                chatty_protocol::AdapterStatus::Offline
            );
            assert_eq!(monitor.adapter_latency_ms, Some(2000));
        }
        _ => panic!("wrong response variant"),
    }
}
