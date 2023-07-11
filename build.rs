fn main() {
    tonic_build::configure()
        .build_server(true)
        .compile(
            &["proto/sdk/alpha/alpha.proto", "proto/sdk/sdk.proto"],
            &[
                "proto/googleapis",
                "proto/grpc-gateway",
                "proto/sdk/alpha",
                "proto/sdk",
            ],
        )
        .expect("failed to compile protobuffers");
}
