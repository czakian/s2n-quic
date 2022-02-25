use netbench::{
    scenario::{self, Scenario},
    Result,
};
use s2n_quic::{
    provider::{
        io,
        tls::default::certificate::{Certificate, IntoCertificate},
    },
    Connection,
};
use std::{collections::HashSet, path::PathBuf, sync::Arc};
use structopt::StructOpt;

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    Client::from_args().run().await
}

#[derive(Debug, StructOpt)]
pub struct Client {
    #[structopt(long, env = "CA")]
    ca: Option<PathBuf>,

    #[structopt(long, default_value = "netbench")]
    application_protocols: Vec<String>,

    #[structopt(long)]
    disable_gso: bool,

    #[structopt(short, long, default_value = "::")]
    local_ip: std::net::IpAddr,

    #[structopt(long, default_value = "0", env = "CLIENT_ID")]
    client_id: usize,

    #[structopt(env = "SCENARIO")]
    scenario: PathBuf,
}

impl Client {
    pub async fn run(&self) -> Result<()> {
        let mut scenario = Scenario::open(&self.scenario)?;
        let mut scenario = scenario.clients.remove(self.client_id);
        let connections: Arc<[_]> = scenario
            .connections
            .drain(..)
            .map(|scenario| Arc::new(scenario))
            .collect::<Vec<_>>()
            .into();

        let mut client = self.client()?;

        // TODO execute client ops instead
        for conn in connections.iter() {
            let addr = std::env::var(format!("SERVER_0"))?;
            let addr = tokio::net::lookup_host(addr)
                .await?
                .next()
                .expect("invalid addr");
            // TODO format the server's connection id as part of the hostname
            let hostname = format!("localhost");
            let connect = s2n_quic::client::Connect::new(addr).with_server_name(hostname);
            eprintln!("connecting to {}", connect);
            let connection = client.connect(connect).await?;
            eprintln!("connected!");
            handle_connection(connection, conn.clone()).await?;
        }

        async fn handle_connection(
            connection: Connection,
            scenario: Arc<scenario::Connection>,
        ) -> Result<()> {
            let conn_id = connection.id();
            let conn =
                netbench::Driver::new(&scenario, netbench::s2n_quic::Connection::new(connection));

            // let mut trace = netbench::trace::Disabled::default();
            // let mut trace = netbench::trace::StdioLogger::new(conn_id, &[][..]);

            let mut trace = netbench::trace::Throughput::default();
            let reporter = trace.reporter(core::time::Duration::from_secs(1));
            let mut checkpoints = HashSet::new();
            let mut timer = netbench::timer::Tokio::default();

            conn.run(&mut trace, &mut checkpoints, &mut timer).await?;

            drop(reporter);

            Ok(())
        }

        client.wait_idle().await?;

        return Ok(());
    }

    fn client(&self) -> Result<s2n_quic::Client> {
        let ca = self.ca()?;

        let tls = s2n_quic::provider::tls::default::Client::builder()
            .with_certificate(ca)?
            // the "amplificationlimit" tests generates a very large chain so bump the limit
            .with_max_cert_chain_depth(10)?
            .with_application_protocols(self.application_protocols.iter().map(String::as_bytes))?
            .with_key_logging()?
            .build()?;

        let mut io_builder =
            io::Default::builder().with_receive_address((self.local_ip, 0u16).into())?;

        if self.disable_gso {
            io_builder = io_builder.with_gso_disabled()?;
        }

        let io = io_builder.build()?;

        let client = s2n_quic::Client::builder()
            .with_io(io)?
            .with_tls(tls)?
            .start()
            .unwrap();

        Ok(client)
    }

    fn ca(&self) -> Result<Certificate> {
        Ok(if let Some(pathbuf) = self.ca.as_ref() {
            pathbuf.into_certificate()?
        } else {
            s2n_quic_core::crypto::tls::testing::certificates::CERT_PEM.into_certificate()?
        })
    }
}
