extern crate prost_build;
// extern crate protobuf_src;

use std::env;
use std::process::Command;

fn find_protoc() -> Option<String> {
    // 1) Wenn PROTOC in der Umgebung gesetzt ist, verwende das
    if let Ok(p) = env::var("PROTOC") {
        return Some(p);
    }

    // 2) Falls nicht: probiere "protoc" aus dem PATH (prüfen ob --version klappt)
    if let Ok(output) = Command::new("protoc").arg("--version").output() {
        if output.status.success() {
            return Some("protoc".to_string());
        }
    }

    // 3) Nichts gefunden
    None
}
fn main() {
    // we use the protobuf-src which provides the protoc compiler. This line makes it available
    // to prost-build
    // setze PROTOC für nachfolgende build-tools (prost-build/tonic-build) wenn gefunden
    if let Some(p) = find_protoc() {
        println!("cargo:warning=Using protoc at: {}", p);
        env::set_var("PROTOC", &p);
    } else {
        // Hilfreiche Fehlermeldung zur Benutzerführung
        panic!(
            "protoc not found: set the PROTOC env var to the path of protoc.exe \
             or install protoc and make it available on PATH. Example (PowerShell): \
             $env:PROTOC='D:\\ProgramingPrograms\\Protoc\\bin\\protoc.exe'"
        );
    }

    let proto_files = [
        "src/simulation/io/proto/types/general.proto",
        "src/simulation/io/proto/types/events.proto",
        "src/simulation/io/proto/types/ids.proto",
        "src/simulation/io/proto/types/network.proto",
        "src/simulation/io/proto/types/population.proto",
        "src/simulation/io/proto/types/vehicles.proto",
        "src/external_services/routing/routing.proto",
        "src/external_services/event_sharing/event_sharing.proto",
    ];

    // tell cargo to rerun this build script if any of the proto files change
    for proto in &proto_files {
        println!("cargo:rerun-if-changed={}", proto);
    }

    // Compiling the protobuf files
    tonic_build::configure()
        .build_client(true)
        .compile_protos(&proto_files, &["src/"])
        .unwrap();
}
