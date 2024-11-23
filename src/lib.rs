use pyo3::prelude::*;
use pyo3::types::{PyDict, IntoPyDict};
use pyo3::exceptions::PyRuntimeError;
use hyper::{Client, Request, Body};
use hyper::client::HttpConnector;
use hyper_tls::HttpsConnector;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::runtime::Runtime;
use once_cell::sync::Lazy;
use encoding_rs::WINDOWS_1252;
use bytes::Bytes;
use flate2::read::{GzDecoder, DeflateDecoder};
use brotli::Decompressor;
use std::io::Read;

static RUNTIME: Lazy<Runtime> = Lazy::new(|| {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder
        .enable_all()
        .worker_threads(num_cpus::get())
        .max_blocking_threads(num_cpus::get() * 2)
        .thread_name("hyper-worker")
        .thread_stack_size(3 * 1024 * 1024);

    #[cfg(unix)]
    builder.on_thread_start(|| {
        use libc::{RLIMIT_NOFILE, rlimit, setrlimit};
        unsafe {
            let mut rlim = rlimit { rlim_cur: 0, rlim_max: 0 };
            if getrlimit(RLIMIT_NOFILE, &mut rlim) == 0 {
                rlim.rlim_cur = rlim.rlim_max;
                setrlimit(RLIMIT_NOFILE, &rlim);
            }
        }
    });

    builder.build().unwrap()
});

static CLIENT: Lazy<Arc<Client<HttpsConnector<HttpConnector>>>> = Lazy::new(|| {
    let mut http = HttpConnector::new();
    http.set_nodelay(true);
    http.set_keepalive(Some(std::time::Duration::from_secs(30)));
    http.enforce_http(false);
    
    let https = HttpsConnector::new_with_connector(http);
    
    Arc::new(Client::builder()
        .pool_idle_timeout(std::time::Duration::from_secs(300))
        .pool_max_idle_per_host(1000)
        .http2_initial_stream_window_size(Some(2 * 1024 * 1024))
        .http2_initial_connection_window_size(Some(4 * 1024 * 1024))
        .http2_adaptive_window(true)
        .http2_keep_alive_interval(std::time::Duration::from_secs(20))
        .http2_keep_alive_timeout(std::time::Duration::from_secs(20))
        .build(https))
});

#[pyclass]
pub struct ClientSession {
    client: Arc<Client<HttpsConnector<HttpConnector>>>,
    headers: HashMap<String, String>,
}

#[pymethods]
impl ClientSession {
    #[new]
    fn new() -> Self {
        ClientSession {
            client: CLIENT.clone(),
            headers: HashMap::new(),
        }
    }

    fn __aenter__(slf: Py<Self>, py: Python<'_>) -> PyResult<&PyAny> {
        pyo3_asyncio::tokio::future_into_py(py, async move { Ok(slf) })
    }

    fn __aexit__<'p>(
        &self,
        py: Python<'p>,
        _exc_type: &PyAny,
        _exc_val: &PyAny,
        _exc_tb: &PyAny,
    ) -> PyResult<&'p PyAny> {
        let fut = async move { Ok(false) };
        pyo3_asyncio::tokio::future_into_py(py, fut)
    }

    #[pyo3(text_signature = "($self, url, /, *, headers = None, params = None)")]
    fn get<'p>(
        &self,
        py: Python<'p>,
        url: String,
        headers: Option<HashMap<String, String>>,
        params: Option<HashMap<String, String>>,
    ) -> PyResult<&'p PyAny> {
        let url = if let Some(params) = params {
            format!("{}?{}", url, encode_params(&params))
        } else {
            url
        };

        let mut merged_headers = self.headers.clone();
        if let Some(h) = headers {
            merged_headers.extend(h);
        }

        let client = self.client.clone();
        let fut = async move {
            let req = Request::builder()
                .method("GET")
                .uri(&url)
                .header("accept", "*/*")
                .header("accept-encoding", "gzip, deflate, br")
                .header("connection", "keep-alive")
                .header("user-agent", "sharphttp/2.0");

            let req = merged_headers.iter().fold(req, |req, (k, v)| {
                req.header(k, v)
            });

            let req = req.body(Body::empty())
                .map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))?;

            let resp = client.request(req).await
                .map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))?;

            let status = resp.status();
            let headers: HashMap<_, _> = resp.headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();

            let body = hyper::body::to_bytes(resp.into_body()).await
                .map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))?;

            Ok(Response {
                status: status.as_u16(),
                headers,
                _body: body,
                text: None,
            })
        };

        pyo3_asyncio::tokio::future_into_py(py, fut)
    }

    #[pyo3(text_signature = "($self, url, /, *, data=None, json=None, headers=None, params=None)")]
    fn post<'p>(
        &self,
        py: Python<'p>,
        url: String,
        data: Option<String>,
        json: Option<&PyDict>,
        headers: Option<HashMap<String, String>>,
        params: Option<HashMap<String, String>>,
    ) -> PyResult<&'p PyAny> {
        let url = if let Some(params) = params {
            format!("{}?{}", url, encode_params(&params))
        } else {
            url
        };

        let mut merged_headers = self.headers.clone();
        if let Some(h) = headers {
            merged_headers.extend(h);
        }

        let body = if let Some(json_data) = json {
            merged_headers.insert("content-type".to_string(), "application/json".to_string());
            let json_str = Python::with_gil(|_py| -> PyResult<String> {
                let json_str = json_data.call_method0("__str__")?
                    .extract::<String>()?;
                Ok(json_str)
            }).map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))?;
            Body::from(json_str)
        } else if let Some(data) = data {
            merged_headers.insert("content-type".to_string(), "application/x-www-form-urlencoded".to_string());
            Body::from(data)
        } else {
            Body::empty()
        };

        let client = self.client.clone();
        let fut = async move {
            let req = Request::builder()
                .method("POST")
                .uri(&url)
                .header("accept", "*/*")
                .header("accept-encoding", "gzip, deflate, br")
                .header("connection", "keep-alive")
                .header("user-agent", "sharphttp/2.0");

            let req = merged_headers.iter().fold(req, |req, (k, v)| {
                req.header(k, v)
            });

            let req = req.body(body)
                .map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))?;

            let resp = client.request(req).await
                .map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))?;

            let status = resp.status();
            let headers: HashMap<_, _> = resp.headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();

            let body = hyper::body::to_bytes(resp.into_body()).await
                .map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))?;

            Ok(Response {
                status: status.as_u16(),
                headers,
                _body: body,
                text: None,
            })
        };

        pyo3_asyncio::tokio::future_into_py(py, fut)
    }
}

#[pyclass]
struct Response {
    status: u16,
    headers: HashMap<String, String>,
    _body: Bytes,
    text: Option<String>,
}

#[pymethods]
impl Response {
    #[getter]
    fn status(&self) -> u16 {
        self.status
    }

    fn json<'p>(&self, py: Python<'p>) -> PyResult<&'p PyAny> {
        let body = self._body.clone();
        let headers = self.headers.clone();
        
        let fut = async move {
            decode_body_with_encoding(&body, &headers)
                .map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))
        };

        let text_future = pyo3_asyncio::tokio::future_into_py(py, fut)?;
        let json_module = py.import("json")?;
        let loads = json_module.getattr("loads")?;
        
        let async_parse = py.eval(
            "lambda text_fut, loads: loads(await text_fut)",
            None,
            Some([
                ("text_fut", text_future),
                ("loads", loads),
            ].into_py_dict(py)),
        )?;

        Ok(async_parse)
    }

    fn text<'p>(&self, py: Python<'p>) -> PyResult<&'p PyAny> {
        let body = self._body.clone();
        let headers = self.headers.clone();
        let fut = async move {
            decode_body_with_encoding(&body, &headers)
                .map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))
        };
        pyo3_asyncio::tokio::future_into_py(py, fut)
    }
}

fn encode_params(params: &HashMap<String, String>) -> String {
    params.iter()
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn decode_body(bytes: &Bytes) -> Result<String, String> {
    if let Ok(text) = String::from_utf8(bytes.to_vec()) {
        return Ok(text);
    }

    let (cow, _, had_errors) = WINDOWS_1252.decode(bytes);
    if !had_errors {
        return Ok(cow.into_owned());
    }

    Ok(String::from_utf8_lossy(bytes).into_owned())
}

fn decode_body_with_encoding(bytes: &Bytes, headers: &HashMap<String, String>) -> Result<String, String> {
    if let Some(encoding) = headers.get("content-encoding") {
        match encoding.as_str() {
            "gzip" => {
                let mut decoder = GzDecoder::new(bytes.as_ref());
                let mut decoded = Vec::new();
                decoder.read_to_end(&mut decoded)
                    .map_err(|e| e.to_string())?;
                return decode_text(&decoded);
            }
            "deflate" => {
                let mut decoder = DeflateDecoder::new(bytes.as_ref());
                let mut decoded = Vec::new();
                decoder.read_to_end(&mut decoded)
                    .map_err(|e| e.to_string())?;
                return decode_text(&decoded);
            }
            "br" => {
                let mut decoder = Decompressor::new(bytes.as_ref(), 4096);
                let mut decoded = Vec::new();
                decoder.read_to_end(&mut decoded)
                    .map_err(|e| e.to_string())?;
                return decode_text(&decoded);
            }
            _ => {}
        }
    }
    
    decode_text(bytes)
}

fn decode_text(bytes: &[u8]) -> Result<String, String> {
    if let Ok(text) = String::from_utf8(bytes.to_vec()) {
        return Ok(text);
    }

    let (cow, _, had_errors) = WINDOWS_1252.decode(bytes);
    if !had_errors {
        return Ok(cow.into_owned());
    }

    Ok(String::from_utf8_lossy(bytes).into_owned())
}

#[pymodule]
fn sharphttp(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<ClientSession>()?;
    Ok(())
}
