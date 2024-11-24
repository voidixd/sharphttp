use pyo3::prelude::*;
use pyo3::types::PyDict;
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
use cookie_store::{CookieStore, Cookie};
use url::Url;
use bytes::BytesMut;
use parking_lot::RwLock;
use smallvec::SmallVec;

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

static BUFFER_POOL: Lazy<RwLock<Vec<BytesMut>>> = Lazy::new(|| {
    RwLock::new(Vec::with_capacity(32))
});

static CLIENT: Lazy<Arc<Client<HttpsConnector<HttpConnector>>>> = Lazy::new(|| {
    let mut http = HttpConnector::new();
    http.set_nodelay(true);
    http.set_keepalive(Some(std::time::Duration::from_secs(30)));
    http.enforce_http(false);
    
    let https = HttpsConnector::new_with_connector(http);
    
    Arc::new(Client::builder()
        .pool_idle_timeout(std::time::Duration::from_secs(300))
        .pool_max_idle_per_host(2000)
        .http2_initial_stream_window_size(Some(4 * 1024 * 1024))
        .http2_initial_connection_window_size(Some(8 * 1024 * 1024))
        .http2_adaptive_window(true)
        .build(https))
});

#[pyclass]
pub struct ClientSession {
    client: Arc<Client<HttpsConnector<HttpConnector>>>,
    headers: SmallVec<[(String, String); 8]>,
    cookie_store: Arc<parking_lot::RwLock<CookieStore>>,
    proxy: Option<String>,
}

#[pymethods]
impl ClientSession {
    #[new]
    #[pyo3(signature = (*, headers=None, cookies=None, proxy=None))]
    fn new(
        headers: Option<HashMap<String, String>>,
        cookies: Option<HashMap<String, String>>,
        proxy: Option<String>,
    ) -> Self {
        let headers = headers
            .map(|h| hashmap_to_smallvec::<8>(h))
            .unwrap_or_default();

        let session = ClientSession {
            client: CLIENT.clone(),
            headers,
            cookie_store: Arc::new(parking_lot::RwLock::new(CookieStore::default())),
            proxy,
        };

        if let Some(cookies) = cookies {
            let base_url = Url::parse("https://example.com").unwrap();
            for (name, value) in cookies {
                let cookie = format!("{}={}; Domain=.example.com; Path=/", name, value);
                if let Ok(cookie) = Cookie::parse(&cookie, &base_url) {
                    session.cookie_store.write().insert_raw(&cookie, &base_url).ok();
                }
            }
        }

        session
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

        let mut merged_headers = SmallVec::<[(String, String); 16]>::new();
        if let Some(h) = headers {
            merged_headers.extend(h.into_iter());
        }

        let client = self.client.clone();
        let cookie_store = self.cookie_store.clone();
        let proxy = self.proxy.clone();
        
        let fut = async move {
            let parsed_url = Url::parse(&url)
                .map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))?;

            let cookie_header = {
                let store = cookie_store.read();
                let cookies: Vec<String> = store
                    .get_request_values(&parsed_url)
                    .map(|(name, value)| format!("{}={}", name, value))
                    .collect();
                cookies.join("; ")
            };

            let mut req = Request::builder()
                .method("GET")
                .uri(&url)
                .header("accept", "*/*")
                .header("accept-encoding", "gzip, deflate, br")
                .header("connection", "keep-alive")
                .header("user-agent", "sharphttp/2.0")
                .header("cookie", cookie_header);

            if let Some(proxy_url) = proxy {
                req = req.header("proxy-authorization", format!("Basic {}", proxy_url));
            }

            let req = merged_headers.iter().fold(req, |req, (k, v)| {
                req.header(k, v)
            });

            let req = req.body(Body::empty())
                .map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))?;

            let resp = client.request(req).await
                .map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))?;

            let status = resp.status();
            let mut response_headers = SmallVec::<[(String, String); 16]>::new();
            for (key, value) in resp.headers() {
                response_headers.push((
                    key.as_str().to_string(),
                    value.to_str().unwrap_or("").to_string()
                ));
            }

            if let Some((_, cookie_value)) = response_headers.iter().find(|(k, _)| k == "set-cookie") {
                if let Ok(cookie) = Cookie::parse(cookie_value, &parsed_url) {
                    cookie_store.write().insert_raw(&cookie, &parsed_url).ok();
                }
            }

            let body = hyper::body::to_bytes(resp.into_body()).await
                .map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))?;

            Python::with_gil(|py| -> PyResult<PyObject> {
                let response = Response {
                    status: status.as_u16(),
                    headers: response_headers,
                    _body: body,
                    text: None,
                };
                Ok(Py::new(py, response)?.into_py(py))
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

        let mut merged_headers = SmallVec::<[(String, String); 16]>::new();
        if let Some(h) = headers {
            merged_headers.extend(h.into_iter());
        }

        let body = if let Some(json_data) = json {
            merged_headers.push((
                "content-type".to_string(),
                "application/json".to_string()
            ));
            let json_str = Python::with_gil(|_py| -> PyResult<String> {
                let json_str = json_data.call_method0("__str__")?
                    .extract::<String>()?;
                Ok(json_str)
            }).map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))?;
            Body::from(json_str)
        } else if let Some(data) = data {
            merged_headers.push((
                "content-type".to_string(),
                "application/x-www-form-urlencoded".to_string()
            ));
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
            let mut response_headers = SmallVec::<[(String, String); 16]>::new();
            for (key, value) in resp.headers() {
                response_headers.push((
                    key.as_str().to_string(),
                    value.to_str().unwrap_or("").to_string()
                ));
            }

            let body = hyper::body::to_bytes(resp.into_body()).await
                .map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))?;

            Python::with_gil(|py| {
                Py::new(py, Response {
                    status: status.as_u16(),
                    headers: response_headers,
                    _body: body,
                    text: None,
                })
            })
        };

        pyo3_asyncio::tokio::future_into_py(py, fut)
    }

    fn get_cookies(&self, py: Python) -> PyResult<PyObject> {
        let cookies: HashMap<String, String> = self.cookie_store
            .read()
            .iter_any()
            .map(|c| (c.name().to_string(), c.value().to_string()))
            .collect();
        Ok(cookies.into_py(py))
    }
}

#[pyclass]
struct Response {
    status: u16,
    headers: SmallVec<[(String, String); 16]>,
    _body: Bytes,
    text: Option<Arc<String>>,
}

#[pymethods]
impl Response {
    #[getter]
    fn status(&self) -> u16 {
        self.status
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

    fn json<'p>(&self, py: Python<'p>) -> PyResult<&'p PyAny> {
        let body = self._body.clone();
        let headers = self.headers.clone();
        
        let fut = async move {
            let text = decode_body_with_encoding(&body, &headers)
                .map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))?;
            
            let value: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| PyErr::new::<PyRuntimeError, _>(e.to_string()))?;
            
            Python::with_gil(|py| {
                match value {
                    serde_json::Value::Null => Ok(py.None().into_py(py)),
                    serde_json::Value::Bool(b) => Ok(b.into_py(py)),
                    serde_json::Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            Ok(i.into_py(py))
                        } else if let Some(f) = n.as_f64() {
                            Ok(f.into_py(py))
                        } else {
                            Ok(n.to_string().into_py(py))
                        }
                    },
                    serde_json::Value::String(s) => Ok(s.into_py(py)),
                    serde_json::Value::Array(arr) => {
                        let py_list = arr.into_iter()
                            .map(|v| v.to_string().into_py(py))
                            .collect::<Vec<_>>()
                            .into_py(py);
                        Ok(py_list)
                    },
                    serde_json::Value::Object(obj) => {
                        let py_dict = obj.into_iter()
                            .map(|(k, v)| (k, v.to_string()))
                            .collect::<HashMap<_, _>>()
                            .into_py(py);
                        Ok(py_dict)
                    },
                }
            })
        };

        pyo3_asyncio::tokio::future_into_py(py, fut)
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
        pyo3_asyncio::tokio::future_into_py(py, async move { Ok(false) })
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

fn decode_body_with_encoding(
    bytes: &Bytes,
    headers: &SmallVec<[(String, String); 16]>
) -> Result<String, String> {
    if let Some((_, encoding)) = headers.iter().find(|(k, _)| k == "content-encoding") {
        let mut buffer = Vec::with_capacity(bytes.len() * 2);
        
        match encoding.as_str() {
            "gzip" => {
                let mut decoder = GzDecoder::new(bytes.as_ref());
                decoder.read_to_end(&mut buffer)
                    .map_err(|e| e.to_string())?;
                decode_text(&buffer)
            }
            "deflate" => {
                let mut decoder = DeflateDecoder::new(bytes.as_ref());
                decoder.read_to_end(&mut buffer)
                    .map_err(|e| e.to_string())?;
                decode_text(&buffer)
            }
            "br" => {
                let mut decoder = Decompressor::new(bytes.as_ref(), 4096);
                decoder.read_to_end(&mut buffer)
                    .map_err(|e| e.to_string())?;
                decode_text(&buffer)
            }
            _ => decode_text(bytes)
        }
    } else {
        decode_text(bytes)
    }
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

fn hashmap_to_smallvec<const N: usize>(map: HashMap<String, String>) -> SmallVec<[(String, String); N]> {
    let mut vec = SmallVec::new();
    vec.extend(map.into_iter());
    vec
}

fn get_header_value<'a>(headers: &'a SmallVec<[(String, String); 16]>, key: &str) -> Option<&'a String> {
    headers.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
}

#[pymodule]
fn sharphttp(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<ClientSession>()?;
    m.add_class::<Response>()?;
    Ok(())
}
