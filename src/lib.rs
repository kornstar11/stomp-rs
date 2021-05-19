#![crate_name = "stomp"]
#![crate_type = "lib"]

#![deny(unused, dead_code, non_snake_case)]

#[macro_use]
extern crate log;
#[macro_use]
extern crate nom;

pub mod connection;
pub mod header;
mod codec;
pub mod frame;
pub mod session;
pub mod subscription;
pub mod transaction;
pub mod message_builder;
pub mod session_builder;
pub mod subscription_builder;
pub mod option_setter;
pub mod errors;


pub use session_builder::SessionBuilder;
