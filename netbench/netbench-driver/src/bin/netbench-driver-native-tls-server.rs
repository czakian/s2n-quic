use netbench::{
    scenario::{self, Scenario},
    Result,
};
use std::{collections::HashSet, path::PathBuf, sync::Arc};
use structopt::StructOpt;
use tokio::{
    net::{TcpListener, TcpStream},
    spawn,
};
use tokio_native_tls::native_tls::{Identity, TlsAcceptor};

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

    #[structopt(long)]
    certificate: Option<PathBuf>,

    #[structopt(long)]
    private_key: Option<PathBuf>,

    #[structopt(long, default_value = "netbench")]
    application_protocols: Vec<String>,

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

        let ident = self.identity()?;
        let acceptor = TlsAcceptor::builder(ident).build()?;
        let acceptor: tokio_native_tls::TlsAcceptor = acceptor.into();
        let acceptor = Arc::new(acceptor);

        let mut conn_id = 0;
        loop {
            let (connection, _addr) = server.accept().await?;
            // spawn a task per connection
            let scenario = scenario.clone();
            let id = conn_id;
            conn_id += 1;
            let acceptor = acceptor.clone();
            spawn(async move {
                if let Err(err) = handle_connection(acceptor, connection, id, scenario).await {
                    eprintln!("{}", err);
                }
            });
        }

        async fn handle_connection(
            acceptor: Arc<tokio_native_tls::TlsAcceptor>,
            connection: TcpStream,
            conn_id: u64,
            scenario: Arc<[scenario::Connection]>,
        ) -> Result<()> {
            let connection = acceptor.accept(connection).await?;

            // TODO parse the hostname
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

            // drop(reporter);

            Ok(())
        }
    }

    async fn server(&self) -> Result<TcpListener> {
        let server = TcpListener::bind((self.ip, self.port)).await?;

        eprintln!("Server listening on port {}", self.port);

        Ok(server)
    }

    fn identity(&self) -> Result<Identity> {
        let ca = if let Some(path) = self.certificate.as_ref() {
            let pem = std::fs::read_to_string(path)?;
            openssl::x509::X509::from_pem(pem.as_bytes())?
        } else {
            openssl::x509::X509::from_pem(
                s2n_quic_core::crypto::tls::testing::certificates::CERT_PEM.as_bytes(),
            )?
        };

        let key = if let Some(path) = self.private_key.as_ref() {
            let pem = std::fs::read_to_string(path)?;
            openssl::pkey::PKey::private_key_from_pem(pem.as_bytes())?
        } else {
            openssl::pkey::PKey::private_key_from_pem(
                s2n_quic_core::crypto::tls::testing::certificates::KEY_PEM.as_bytes(),
            )?
        };

        let cert = openssl::pkcs12::Pkcs12::builder().build("", "", &key, &ca)?;
        let cert = cert.to_der()?;

        let ident = Identity::from_pkcs12(&cert, "")?;

        Ok(ident)
    }
}
