use crate::{
    error::Errno,
    fs::File,
    header::fcntl,
    io::Read,
};
use alloc::string::String;

pub fn get_dns_server() -> Result<String, Errno> {
    let mut string = String::new();
    // dhcpd publishes the lease here so callers need no netcfg capability.
    if let Ok(mut file) = File::open(c"/etc/net/dns".into(), fcntl::O_RDONLY)
        && file.read_to_string(&mut string).is_ok()
        && super::lookup::parse_ipv4_string(string.trim()).is_some()
    {
        return Ok(string);
    }
    Ok(String::from("9.9.9.9"))
}
