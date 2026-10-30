mod connection;
pub(crate) use connection::handle_connection_with_route;

pub use connection::{
    HOST_STOPPED, MAX_FRAME_BYTES, PRE_IDENTIFY_FRAME_BYTES, RATE_LIMITED, handle_connection,
};
