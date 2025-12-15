use clap::Parser;
use rust_qsim::external_services::event_sharing::{EventSharingServiceAdapterFactory};
use rust_qsim::external_services::{AdapterHandleBuilder, AsyncExecutor, ExternalServiceType};
use rust_qsim::simulation::config::Config;
use rust_qsim::simulation::controller;
use rust_qsim::simulation::controller::local_controller::LocalControllerBuilder;
use rust_qsim::simulation::controller::ExternalServices;
use rust_qsim::simulation::logging::init_std_out_logging_thread_local;
use rust_qsim::simulation::scenario::GlobalScenario;
use std::sync::{Arc, Barrier};
use rust_qsim::external_services::{
    event_sharing::event_sharing_logger::make_event_sharing_subscriber
    };

use std::collections::HashMap;

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct RoutingCommandLineArgs {
    #[arg(long, short)]
    router_ip: String,
    #[clap(flatten)]
    delegate: rust_qsim::simulation::config::CommandLineArgs,
}

fn main() {
    let _guard = init_std_out_logging_thread_local();
    let args = RoutingCommandLineArgs::parse();
    let config = Arc::new(Config::from(args.delegate));

    // Creating the routing adapter is only one task, so we add 1 and not the number of worker threads!
    let total_thread_count = config.partitioning().num_parts + 1;
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

    let event_sharing_executor = AsyncExecutor::from_config(&config, barrier.clone());
    let event_sharing_factory = EventSharingServiceAdapterFactory::new(
        vec![&args.router_ip],
        config.clone(),
        event_sharing_executor.shutdown_handles(),
    ).with_batch_params(10000, 10);

    // Spawning the routing service adapter in a separate thread. The adapter will be run in its own tokio runtime.
    // This function returns
    // - the join handle of the adapter thread
    // - a channel for sending requests to the adapter
    // - a channel for sending shutdown signal for the adapter

    let (event_sharing_handle, event_sharing_send, event_sharing_send_sd) = event_sharing_executor.spawn_thread("event_sharing", event_sharing_factory);

    // The request sender is passed to the controller.

    let mut services = ExternalServices::default();
    services.insert(ExternalServiceType::EventSharing("event_sharing".into()), event_sharing_send.clone().into());

    // Load scenario
    let scenario = GlobalScenario::load(config.clone());

    // Build a HashMap<u32, Vec<Box<OnEventFnBuilder>>> with one subscriber per partition.
    // NB: Box<dyn FnOnce(..)> is not Clone, so we call make_event_sharing_subscriber(...) once per partition.
    let mut events_subscribers_per_partition: HashMap<u32, Vec<Box<rust_qsim::simulation::events::OnEventFnBuilder>>> =
        HashMap::new();

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
        vec![AdapterHandleBuilder::default()
            .shutdown_sender(event_sharing_send_sd)
            .handle(event_sharing_handle)
            .build()
            .unwrap()],
    )
}
