use netbench::{
    scenario::{self, Scenario},
    Result,
};
use std::{collections::HashSet, net::SocketAddr, path::PathBuf, sync::Arc};
use structopt::StructOpt;
use tokio::{
    net::{TcpListener, TcpStream},
    spawn,
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    Server::from_args().run().await
}

#[derive(Debug, StructOpt)]
pub struct Server {
    #[structopt(short, long, default_value = "::")]
    ip: std::net::IpAddr,

    #[structopt(short, long, default_value = "4433")]
    port: u16,

    #[structopt(long, default_value = "0")]
    server_id: usize,

    scenario: PathBuf,
}

impl Server {
    pub async fn run(&self) -> Result<()> {
        let mut scenario = Scenario::open(&self.scenario)?;
        let scenario = scenario.servers.remove(self.server_id);
        let scenario: Arc<[_]> = scenario.connections.clone().into();

        let server = self.server().await?;

        let mut conn_id = 0;
        loop {
            let (connection, _addr) = server.accept().await?;
            // spawn a task per connection
            let scenario = scenario.clone();
            let id = conn_id;
            conn_id += 1;
            spawn(async move {
                let _ = dbg!(handle_connection(connection, id, scenario).await);
            });
        }

        async fn handle_connection(
            connection: TcpStream,
            conn_id: u64,
            scenario: Arc<[scenario::Connection]>,
        ) -> Result<()> {
            // TODO parse the first few bytes for the server id
            let id = 0;
            let scenario = scenario.get(id).ok_or("invalid connection id")?;

            let config = Default::default();
            let connection = Box::pin(connection);

            let conn = netbench::Driver::new(
                scenario,
                netbench::multiplex::Connection::new(connection, config),
            );

            // let mut trace = netbench::trace::Disabled::default();
            let mut trace = netbench::trace::StdioLogger::new(conn_id, &[][..]);

            // let mut trace = netbench::trace::Throughput::default();
            // let reporter = trace.reporter(core::time::Duration::from_secs(1));

            let mut checkpoints = HashSet::new();
            let mut timer = netbench::timer::Tokio::default();

            conn.run(&mut trace, &mut checkpoints, &mut timer).await?;

            // drop(trace);

            Ok(())
        }
    }

    async fn server(&self) -> Result<TcpListener> {
        let server = TcpListener::bind((self.ip, self.port)).await?;

        eprintln!("Server listening on port {}", self.port);

        Ok(server)
    }
}
