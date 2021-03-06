use futures::Future;
use tokio::runtime::current_thread::Runtime;
use tokio_postgres::{Client, Connection, Error, NoTls, Socket};

fn connect(s: &str) -> impl Future<Item = (Client, Connection<Socket>), Error = Error> {
    s.parse::<tokio_postgres::Builder>().unwrap().connect(NoTls)
}

fn smoke_test(s: &str) {
    let mut runtime = Runtime::new().unwrap();
    let connect = connect(s);
    let (mut client, connection) = runtime.block_on(connect).unwrap();
    let connection = connection.map_err(|e| panic!("{}", e));
    runtime.spawn(connection);

    let execute = client.batch_execute("SELECT 1");
    runtime.block_on(execute).unwrap();
}

#[test]
#[ignore] // FIXME doesn't work with our docker-based tests :(
fn unix_socket() {
    smoke_test("host=/var/run/postgresql port=5433 user=postgres");
}

#[test]
fn tcp() {
    smoke_test("host=localhost port=5433 user=postgres")
}

#[test]
fn multiple_hosts_one_port() {
    smoke_test("host=foobar.invalid,localhost port=5433 user=postgres");
}

#[test]
fn multiple_hosts_multiple_ports() {
    smoke_test("host=foobar.invalid,localhost port=5432,5433 user=postgres");
}

#[test]
fn wrong_port_count() {
    let mut runtime = Runtime::new().unwrap();
    let f = connect("host=localhost port=5433,5433 user=postgres");
    runtime.block_on(f).err().unwrap();

    let f = connect("host=localhost,localhost,localhost port=5433,5433 user=postgres");
    runtime.block_on(f).err().unwrap();
}
