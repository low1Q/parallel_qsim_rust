pub fn performance_logging_enabled() -> bool {
    std::env::var("ENABLE_PERFORMANCE_LOGGING")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub fn only_route_blocking_wait_enabled() -> bool {
    std::env::var("ONLY_ROUTE_BLOCKING_WAIT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub fn any_measurement_enabled() -> bool {
    performance_logging_enabled() || only_route_blocking_wait_enabled()
}
