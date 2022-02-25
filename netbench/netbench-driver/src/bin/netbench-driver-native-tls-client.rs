use netbench::{
    scenario::{self, Scenario},
    Result,
};
use std::{collections::HashSet, net::SocketAddr, path::PathBuf, sync::Arc};
use structopt::StructOpt;
use tokio::net::TcpStream;
use tokio_native_tls::native_tls::{Certificate, TlsConnector};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    Client::from_args().run().await
}

#[derive(Debug, StructOpt)]
pub struct Client {
    #[structopt(long)]
    ca: Option<PathBuf>,

    #[structopt(long, default_value = "netbench")]
    application_protocols: Vec<String>,

    #[structopt(long, default_value = "0")]
    client_id: usize,

    scenario: PathBuf,
}

impl Client {
    pub async fn run(&self) -> Result<()> {
        let mut scenario = Scenario::open(&self.scenario)?;
        let mut scenario = scenario.clients.remove(self.client_id);
        let scenario: Arc<[_]> = scenario
            .connections
            .drain(..)
            .map(|scenario| Arc::new(scenario))
            .collect::<Vec<_>>()
            .into();

        let connector = TlsConnector::builder()
            .add_root_certificate(self.ca()?)
            .build()?;
        let connector: tokio_native_tls::TlsConnector = connector.into();
        let connector = Arc::new(connector);

        // TODO execute client ops instead
        let mut conn_id = 0;
        for scenario in scenario.iter() {
            // TODO read server address from instance file
            let addr: SocketAddr = "192.168.86.76:4433".parse()?;
            let connection = TcpStream::connect(addr).await?;
            let id = conn_id;
            conn_id += 1;
            handle_connection(connector.clone(), connection, id, scenario.clone()).await?;
        }

        async fn handle_connection(
            connector: Arc<tokio_native_tls::TlsConnector>,
            connection: TcpStream,
            conn_id: u64,
            scenario: Arc<scenario::Connection>,
        ) -> Result<()> {
            // TODO write the server's connection id
            let connection = connector.connect("localhost", connection).await?;

            let config = Default::default();
            let connection = Box::pin(connection);

            let conn = netbench::Driver::new(
                &scenario,
                netbench::multiplex::Connection::new(connection, config),
            );

            // let mut trace = netbench::trace::Disabled::default();
            let mut trace = netbench::trace::StdioLogger::new(conn_id, &[][..]);

            // let mut trace = netbench::trace::Throughput::default();
            // let reporter = trace.reporter(core::time::Duration::from_secs(1));
            let mut checkpoints = HashSet::new();
            let mut timer = netbench::timer::Tokio::default();

            conn.run(&mut trace, &mut checkpoints, &mut timer).await?;

            // drop(reporter);

            Ok(())
        }

        return Ok(());
    }

    fn ca(&self) -> Result<Certificate> {
        Ok(if let Some(path) = self.ca.as_ref() {
            let key = std::fs::read(path)?;
            Certificate::from_pem(&key)?
        } else {
            Certificate::from_pem(
                s2n_quic_core::crypto::tls::testing::certificates::CERT_PEM.as_bytes(),
            )?
        })
    }
}
