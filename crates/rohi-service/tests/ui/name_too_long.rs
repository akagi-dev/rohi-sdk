use rohi_service::Service;

#[derive(Service)]
#[service(name = "a_name_that_is_far_too_long_for_the_wire")]
struct LongService;

fn main() {}
