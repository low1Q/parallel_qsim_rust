use clap::Parser;
use rust_qsim::external_services::event_sharing::event_sharing_logger::{make_event_sharing_subscriber, print_event_sharing_stats};
use rust_qsim::external_services::event_sharing::EventSharingServiceAdapterFactory;
use rust_qsim::external_services::{AdapterHandleBuilder, AsyncExecutor, ExternalServiceType};
use rust_qsim::simulation::config::Config;
use rust_qsim::simulation::controller;
use rust_qsim::simulation::controller::local_controller::LocalControllerBuilder;
use rust_qsim::simulation::controller::ExternalServices;
use rust_qsim::simulation::logging::init_std_out_logging_thread_local;
use rust_qsim::simulation::scenario::GlobalScenario;
use std::collections::HashMap;
use std::sync::{Arc, Barrier};

use chrono::Local;
use rust_qsim::external_services::routing::RoutingServiceAdapterFactory;

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct RoutingCommandLineArgs {
    #[arg(long, short)]
    router_ip: String,
    #[clap(flatten)]
    delegate: rust_qsim::simulation::config::CommandLineArgs,
    #[arg(long, default_value_t = 100)]
    event_sharing_bin_size_secs: u32,
    #[arg(long, default_value_t = 10000)]
    event_sharing_closed_bin_batch_size: usize,
    #[arg(long, default_value_t = false)]
    disable_all_measurements: bool,

    #[arg(long, default_value_t = false)]
    only_route_blocking_wait: bool,

    #[arg(long, default_value_t = false)]
    enable_performance_logging: bool,

    #[arg(long, default_value_t = 600)]
    preplanning_horizon: u32,

    #[arg(long, default_value_t = 1)]
    num_routing_threads: u32,

    #[arg(long, default_value = "")]
    custom_string: String,
}

fn main() {
    let _guard = init_std_out_logging_thread_local();
    let args = RoutingCommandLineArgs::parse();
    let config = Arc::new(Config::from(args.delegate));

    let parts = config.partitioning().num_parts;
    let bin_size = args.event_sharing_bin_size_secs;
    let batch_size = args.event_sharing_closed_bin_batch_size;
    let horizon = args.preplanning_horizon.clone();
    let threads = args.num_routing_threads;
    let custom_string = args.custom_string.clone();
    let timestamp = Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();

    if args.disable_all_measurements {
        std::env::set_var("ENABLE_PERFORMANCE_LOGGING", "false");
        std::env::set_var("ONLY_ROUTE_BLOCKING_WAIT", "false");
    } else if args.only_route_blocking_wait {
        std::env::set_var("ENABLE_PERFORMANCE_LOGGING", "false");
        std::env::set_var("ONLY_ROUTE_BLOCKING_WAIT", "true");
    } else if args.enable_performance_logging {
        std::env::set_var("ENABLE_PERFORMANCE_LOGGING", "true");
        std::env::set_var("ONLY_ROUTE_BLOCKING_WAIT", "false");
    } else {
        std::env::set_var("ENABLE_PERFORMANCE_LOGGING", "false");
        std::env::set_var("ONLY_ROUTE_BLOCKING_WAIT", "false");
    }

    let routing_csv = format!(
        "rust-routing-requests-bin{}-threads{}-PH{}-batch{}-parts{}-{}-{}.csv",
        bin_size, threads, horizon, batch_size, parts, custom_string, timestamp
    );
    let event_csv = format!(
        "rust-event-sharing-summary-bin{}-threads{}-PH{}-batch{}-parts{}-{}-{}.csv",
        bin_size, threads, horizon, batch_size, parts, custom_string, timestamp
    );
    let blocking_csv = format!(
        "rust-routing-blocking-wait-bin{}-threads{}-PH{}-batch{}-parts{}-{}-{}.csv",
        bin_size, threads, horizon, batch_size, parts, custom_string, timestamp
    );

    std::env::set_var("ROUTING_RUST_CSV", routing_csv);
    std::env::set_var("EVENT_SHARING_SUMMARY_RUST_CSV", event_csv);
    std::env::set_var("ROUTING_BLOCKING_WAIT_CSV", blocking_csv);

    // Solange der Rank nicht sauber aus der Laufumgebung durchgereicht wird,
    // setzen wir hier einen Default. Der einzelne QSim-Prozess kann das überschreiben.
    std::env::set_var("RUST_QSIM_RANK", "unknown");

    // Creating the routing adapter and the event sharing adapter are only two task, so we add 2 and not the number of worker threads!
    let total_thread_count = config.partitioning().num_parts + 2;
    let barrier = Arc::new(Barrier::new(total_thread_count as usize));

    // Configuring the routing adapter. We need
    // - the IP address of the router service
    // - the configuration of the simulation
    // - the shutdown handles of the executor (= receiver of shutdown signals from the controller)
    // The AsyncExecutor will spawn a thread for the routing service adapter and an async runtime.

    // let executor = AsyncExecutor::from_config(&config, barrier.clone());
    // let factory = RoutingServiceAdapterFactory::new(
    //     vec![&args.router_ip],
    //     config.clone(),
    //     executor.shutdown_handles(),
    // );

    // Car-Routing-Service-Adapter
    let car_routing_executor = AsyncExecutor::from_config(&config, barrier.clone());
    let car_routing_factory = RoutingServiceAdapterFactory::new(
        vec![&args.router_ip],
        config.clone(),
        car_routing_executor.shutdown_handles(),
    );

    // EventSharing-Service-Adapter
    //
    // wichtige Semantik:
    // - Der Adapter segmentiert Events clientseitig in 900s-Time-Bins.
    // - Ein Bin wird erst dann abgeschlossen und publiziert, wenn durch ein späteres Event
    //   sicher ist, dass keine weiteren Events mehr für diesen Bin kommen können.
    // - Die alte batch-/flush-orientierte Logik wird nicht mehr verwendet.
    let event_sharing_executor = AsyncExecutor::from_config(&config, barrier.clone());
    let event_sharing_factory = EventSharingServiceAdapterFactory::new(
        vec![&args.router_ip],
        config.clone(),
        event_sharing_executor.shutdown_handles(),
    )
    .with_bin_size_secs(args.event_sharing_bin_size_secs)
    .with_closed_bin_batch_size(args.event_sharing_closed_bin_batch_size)
    .with_batch_params(10000, 10);

    // Spawning the routing service adapter in a separate thread. The adapter will be run in its own tokio runtime.
    // This function returns
    // - the join handle of the adapter thread
    // - a channel for sending requests to the adapter
    // - a channel for sending shutdown signal for the adapter

    let (car_routing_handle, car_routing_send, car_routing_send_sd) =
        car_routing_executor.spawn_thread("car_router", car_routing_factory);
    let (event_sharing_handle, event_sharing_send, event_sharing_send_sd) =
        event_sharing_executor.spawn_thread("event_sharing", event_sharing_factory);

    // The request sender is passed to the controller.

    let mut services = ExternalServices::default();
    services.insert(
        ExternalServiceType::EventSharing("event_sharing".into()),
        event_sharing_send.clone().into(),
    );
    services.insert(
        ExternalServiceType::Routing("car".into()),
        car_routing_send.clone().into(),
    );

    // Load scenario
    let scenario = GlobalScenario::load(config.clone());

    // Build a HashMap<u32, Vec<Box<OnEventFnBuilder>>> with one subscriber per partition.
    // NB: Box<dyn FnOnce(..)> is not Clone, so we call make_event_sharing_subscriber(...) once per partition.
    let mut events_subscribers_per_partition: HashMap<
        u32,
        Vec<Box<rust_qsim::simulation::events::OnEventFnBuilder>>,
    > = HashMap::new();

    // Wrap the sender in Arc so each subscriber can capture a clone cheaply
    let sender_arc = Arc::new(event_sharing_send.clone());

    let num_parts = config.partitioning().num_parts;
    for part in 0..num_parts {
        // create a new Box<OnEventFnBuilder> for this partition
        let subscriber = make_event_sharing_subscriber(sender_arc.clone());
        events_subscribers_per_partition.insert(part, vec![subscriber]);
    }

    let controller = LocalControllerBuilder::default()
        .global_scenario(scenario)
        .external_services(services)
        .global_barrier(barrier)
        .events_subscriber_per_partition(events_subscribers_per_partition)
        .build()
        .unwrap();
    // ####################################################################################################
    //
    // // Create controller
    // let controller = LocalControllerBuilder::default()
    //     .global_scenario(scenario)
    //     .external_services(services)
    //     .global_barrier(barrier)
    //     .build()
    //     .unwrap();

    // Run controller
    let sim_handles = controller.run();

    // Wait for the controller to finish and the routing adapter to finish.
    controller::try_join(
        sim_handles,
        vec![
            AdapterHandleBuilder::default()
                .shutdown_sender(car_routing_send_sd)
                .handle(car_routing_handle)
                .build()
                .unwrap(),
            AdapterHandleBuilder::default()
                .shutdown_sender(event_sharing_send_sd)
                .handle(event_sharing_handle)
                .build()
                .unwrap(),
        ],
    );

    print_event_sharing_stats();
}
