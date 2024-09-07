use core::str;
use std::{
    error::Error,
    io,
    path::Path,
    process::{Child, Command},
};

use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use clap::Parser;
use fastcgi_client::{Client, Params};
use futures::TryStreamExt;
use nix::unistd::Pid;
use tokio::{fs, net::TcpStream};
use tokio_util::io::StreamReader;

#[derive(Debug, Parser)]
struct Args {
    #[clap(long, default_value_t = String::from("0.0.0.0:3222"))]
    addr: String,

    #[clap(long = "fpm.addr", default_value_t=String::from("127.0.0.1:9000"))]
    fpm_addr: String,

    #[clap(long= "fpm.script_path", default_value_t = String::from("pub/index.php"))]
    fpm_script_path: String,

    #[clap(long = "fpm.config_path", default_value_t=String::from("php-fpm.conf"))]
    fpm_config_path: String,
}

#[derive(Clone)]
struct FpmConfig {
    script_path: String,
    addr: String,
    config_path: String,
}

impl FpmConfig {
    async fn new<P: AsRef<Path>>(
        script_path: P,
        config_path: P,
        addr: &str,
    ) -> Result<Self, Box<dyn Error>> {
        let script_path = fs::canonicalize(script_path)
            .await?
            .to_str()
            .ok_or("invalid unicode")?
            .to_string();
        let config_path = fs::canonicalize(config_path)
            .await?
            .to_str()
            .ok_or("invalid unicode")?
            .to_string();
        Ok(Self {
            script_path,
            config_path,
            addr: addr.to_string(),
        })
    }
}

async fn dispatch_to_fpm(config: &FpmConfig, req: Request) -> Result<Response, Box<dyn Error>> {
    let stream = TcpStream::connect(&config.addr).await?;
    let mut client = Client::new_keep_alive(stream);

    let mut params = Params::default()
        .request_method(req.method().to_string())
        .script_filename(&config.script_path)
        .script_name("/indx.php")
        .request_uri("/")
        .remote_addr("127.0.0.1")
        .remote_port(12345)
        .server_addr("127.0.0.1")
        .server_port(80)
        .server_name("localhost");

    if let Some(v) = req.headers().get(HeaderName::from_static("content-length")) {
        let len = v.to_str()?.parse::<usize>()?;
        params = params.content_length(len);
    }

    if let Some(v) = req.headers().get(HeaderName::from_static("content-type")) {
        params = params.content_type(String::from(v.to_str()?));
    }

    // this is some real bullshit, right? this is how you turn a body into an AsyncRead.
    let s = req
        .into_body()
        .into_data_stream()
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err));
    let br = StreamReader::new(s);
    futures::pin_mut!(br);

    // This is super stupid. So first of all, we read the entire fcgi response into memory, parse out the headers, make another clone
    // of the response just to discard the header previous from the original buffer because we need to return something in the response
    // that it can own.
    let res = client
        .execute(fastcgi_client::Request::new(params, &mut br))
        .await?;

    let out = res.stdout.ok_or("no stdout")?;

    Ok(parse_fpm_response(&out)?.into_response())
}

async fn handler(State(config): State<FpmConfig>, req: Request) -> Response {
    match dispatch_to_fpm(&config, req).await {
        Ok(res) => res,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

struct HeaderIter<'a> {
    data: &'a [u8],
}

impl HeaderIter<'_> {
    fn new(data: &[u8]) -> HeaderIter {
        HeaderIter { data }
    }
}

impl<'a> Iterator for HeaderIter<'a> {
    type Item = Result<(HeaderName, HeaderValue), Box<dyn Error>>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.data.is_empty() {
            return None;
        }

        let sep = vec![b'\r', b'\n'];
        let data = if let Some(ix) = self.data.windows(sep.len()).position(|w| w == sep) {
            let data = &self.data[..ix];
            self.data = &self.data[ix + sep.len()..];
            data
        } else {
            let data = self.data;
            self.data = &[];
            data
        };

        Some(parse_fpm_header(data))
    }
}

fn parse_fpm_header(data: &[u8]) -> Result<(HeaderName, HeaderValue), Box<dyn Error>> {
    let sep = vec![b':', b' '];
    if let Some(ix) = data.windows(sep.len()).position(|w| w == sep) {
        Ok((
            HeaderName::from_bytes(&data[..ix])?,
            HeaderValue::from_bytes(&data[ix + sep.len()..])?,
        ))
    } else {
        Err("invalid header")?
    }
}

fn parse_fpm_response(data: &[u8]) -> Result<(StatusCode, HeaderMap, Vec<u8>), Box<dyn Error>> {
    let sep = vec![b'\r', b'\n', b'\r', b'\n'];
    let ix = data
        .windows(sep.len())
        .position(|w| w == sep)
        .ok_or("headers not found")?;

    let mut status = StatusCode::OK;
    let mut headers = HeaderMap::new();
    for item in HeaderIter::new(&data[..ix]) {
        let (name, value) = item?;
        if name == HeaderName::from_static("status") {
            let code = str::from_utf8(
                &value
                    .as_bytes()
                    .iter()
                    .copied()
                    .take_while(|&c| c.is_ascii_digit())
                    .collect::<Vec<_>>(),
            )?
            .parse::<u16>()?;
            status = StatusCode::from_u16(code)?;
        } else {
            headers.insert(name, value);
        }
    }

    Ok((status, headers, data[ix + sep.len()..].to_vec()))
}

fn run_php_fpm(cfg: &FpmConfig) -> io::Result<Child> {
    Command::new("php-fpm")
        .arg("-n")
        .arg("-y")
        .arg(&cfg.config_path)
        .spawn()
}

fn kill_process_group(proc: &Child) -> Result<(), nix::errno::Errno> {
    let pid = Pid::from_raw(-(proc.id() as i32));
    nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

    let fpm_config =
        FpmConfig::new(args.fpm_script_path, args.fpm_config_path, &args.fpm_addr).await?;

    let fpm_proc = run_php_fpm(&fpm_config)?;
    ctrlc::set_handler(move || {
        let _ = kill_process_group(&fpm_proc);
        std::process::exit(0);
    })?;

    let app = Router::new().fallback(handler).with_state(fpm_config);
    let listener = tokio::net::TcpListener::bind(args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
