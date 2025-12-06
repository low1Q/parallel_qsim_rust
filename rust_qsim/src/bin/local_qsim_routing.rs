use clap::Parser;
use rust_qsim::external_services::routing::RoutingServiceAdapterFactory;
use rust_qsim::external_services::event_sharing::EventSharingServiceAdapterFactory;
use rust_qsim::external_services::{AdapterHandleBuilder, AsyncExecutor, ExternalServiceType};
use rust_qsim::simulation::config::Config;
use rust_qsim::simulation::controller;
use rust_qsim::simulation::controller::local_controller::LocalControllerBuilder;
use rust_qsim::simulation::controller::ExternalServices;
use rust_qsim::simulation::logging::init_std_out_logging_thread_local;
use rust_qsim::simulation::scenario::GlobalScenario;
use std::sync::{Arc, Barrier};

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
    let total_thread_count = config.partitioning().num_parts + 2;
    let barrier = Arc::new(Barrier::new(total_thread_count as usize));

    let car_executor = AsyncExecutor::from_config(&config, barrier.clone());
    let car_factory = RoutingServiceAdapterFactory::new(
        vec![&args.router_ip],
        config.clone(),
        car_executor.shutdown_handles(),
    );

    let event_sharing_executor = AsyncExecutor::from_config(&config, barrier.clone());
    let event_sharing_factory = EventSharingServiceAdapterFactory::new(
        vec![&args.router_ip],
        config.clone(),
        event_sharing_executor.shutdown_handles(),
    );

    // let pt_executor = AsyncExecutor::from_config(&config, barrier.clone());
    // let pt_factory = RoutingServiceAdapterFactory::new(
    //     vec![&args.router_ip],
    //     config.clone(),
    //     pt_executor.shutdown_handles(),
    // );

    let (car_router_handle, car_send, car_send_sd) = car_executor.spawn_thread("car_router", car_factory);
    let (event_sharing_handle, event_sharing_send, event_sharing_send_sd) = event_sharing_executor.spawn_thread("event_sharing", event_sharing_factory);
    // let (pt_router_handle, pt_send, pt_send_sd) = pt_executor.spawn_thread("pt_Router", pt_factory);

    let mut services = ExternalServices::default();
    services.insert(ExternalServiceType::Routing("car".into()), car_send.into());
    // services.insert(ExternalServiceType::Routing("pt".into()), pt_send.into());
    services.insert(ExternalServiceType::EventSharing("event_sharing".into()), event_sharing_send.into());

    let scenario = GlobalScenario::build(config);

    let controller = LocalControllerBuilder::default()
        .global_scenario(scenario)
        .external_services(services)
        .global_barrier(barrier)
        .build()
        .unwrap();

    let sim_handles = controller.run();

    controller::try_join(
        sim_handles,
        vec![AdapterHandleBuilder::default()
            .shutdown_sender(car_send_sd)
            .handle(car_router_handle)
            .build()
            .unwrap(),
             AdapterHandleBuilder::default()
                 .shutdown_sender(event_sharing_send_sd)
                 .handle(event_sharing_handle)
                 .build()
                 .unwrap()],
    )
}
//,
//          AdapterHandleBuilder::default()
//              .shutdown_sender(pt_send_sd)
//              .handle(pt_router_handle)
//              .build()
//              .unwrap()
