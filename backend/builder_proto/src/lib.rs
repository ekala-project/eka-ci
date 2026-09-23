pub mod ekaci {
    pub mod builder {
        pub mod v1 {
            tonic::include_proto!("ekaci.builder.v1");
        }
    }
}

pub use ekaci::builder::v1::*;
