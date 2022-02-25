use netbench::{
    scenario::{self, Scenario},
    Result,
};
use s2n_quic::{
    provider::{
        io,
        tls::default::certificate::{Certificate, IntoCertificate, IntoPrivateKey, PrivateKey},
    },
    Connection,
};
use std::{collections::HashSet, path::PathBuf, sync::Arc};
use structopt::StructOpt;
use tokio::spawn;

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    Server::from_args().run().await
}

#[derive(Debug, StructOpt)]
pub struct Server {
    #[structopt(short, long, default_value = "::")]
    ip: std::net::IpAddr,

    #[structopt(short, long, default_value = "4433", env = "PORT")]
    port: u16,

    #[structopt(long, env = "CERTIFICATE")]
    certificate: Option<PathBuf>,

    #[structopt(long, env = "PRIVATE_KEY")]
    private_key: Option<PathBuf>,

    #[structopt(long, default_value = "netbench")]
    application_protocols: Vec<String>,

    #[structopt(long)]
    disable_gso: bool,

    #[structopt(long, default_value = "0", env = "SERVER_ID")]
    server_id: usize,

    #[structopt(env = "SCENARIO")]
    scenario: PathBuf,
}

impl Server {
    pub async fn run(&self) -> Result<()> {
        let mut scenario = Scenario::open(&self.scenario)?;
        let scenario = scenario.servers.remove(self.server_id);
        let scenario: Arc<[_]> = scenario.connections.clone().into();

        let mut server = self.server()?;

        let trace = netbench::trace::Throughput::default();
        let reporter = trace.reporter(core::time::Duration::from_secs(1));

        while let Some(connection) = server.accept().await {
            // spawn a task per connection
            let scenario = scenario.clone();
            let trace = trace.clone();
            spawn(async move {
                if let Err(error) = handle_connection(connection, scenario, trace).await {
                    eprintln!("{:#}", error);
                }
            });
        }

        drop(reporter);

        return Err("".into());

        async fn handle_connection(
            connection: Connection,
            scenario: Arc<[scenario::Connection]>,
            mut trace: netbench::trace::Throughput,
        ) -> Result<()> {
            // let host = connection.sni()?.ok_or("missing hostname")?;
            // let id = host.split(".").next().ok_or("invalid hostname")?;
            // let id: usize = id.parse()?;
            let id = 0;
            let scenario = scenario.get(id).ok_or("invalid connection id")?;

            let conn =
                netbench::Driver::new(scenario, netbench::s2n_quic::Connection::new(connection));

            let mut checkpoints = HashSet::new();
            let mut timer = netbench::timer::Tokio::default();

            conn.run(&mut trace, &mut checkpoints, &mut timer).await?;

            Ok(())
        }
    }

    fn server(&self) -> Result<s2n_quic::Server> {
        let private_key = self.private_key()?;
        let certificate = self.certificate()?;

        let tls = s2n_quic::provider::tls::default::Server::builder()
            .with_certificate(certificate, private_key)?
            .with_application_protocols(self.application_protocols.iter().map(String::as_bytes))?
            .with_key_logging()?
            .build()?;

        let mut io_builder =
            io::Default::builder().with_receive_address((self.ip, self.port).into())?;

        if self.disable_gso {
            io_builder = io_builder.with_gso_disabled()?;
        }

        let io = io_builder.build()?;

        let server = s2n_quic::Server::builder()
            .with_io(io)?
            .with_tls(tls)?
            .start()
            .unwrap();

        eprintln!("Server listening on port {}", self.port);

        Ok(server)
    }

    fn certificate(&self) -> Result<Certificate> {
        Ok(if let Some(pathbuf) = self.certificate.as_ref() {
            pathbuf.into_certificate()?
        } else {
            s2n_quic_core::crypto::tls::testing::certificates::CERT_PEM.into_certificate()?
        })
    }

    fn private_key(&self) -> Result<PrivateKey> {
        Ok(if let Some(pathbuf) = self.private_key.as_ref() {
            pathbuf.into_private_key()?
        } else {
            s2n_quic_core::crypto::tls::testing::certificates::KEY_PEM.into_private_key()?
        })
    }
}
