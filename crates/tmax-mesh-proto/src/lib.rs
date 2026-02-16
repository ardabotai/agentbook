pub mod mesh {
    pub mod v1 {
        tonic::include_proto!("tmax.mesh.v1");
    }
}

pub mod host {
    #[allow(clippy::large_enum_variant)]
    pub mod v1 {
        tonic::include_proto!("tmax.host.v1");
    }
}
