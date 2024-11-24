# 🚀 SharpHTTP

A blazingly fast HTTP client implementation in Rust for Python, offering exceptional performance and reliability.

[![PyPI version](https://badge.fury.io/py/sharphttp.svg)](https://badge.fury.io/py/sharphttp)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

## ✨ Features

- 🏃‍♂️ **Lightning Fast**: Up to 3.3x faster than aiohttp
- 🔒 **Secure**: Built on top of hyper-tls
- 🔄 **Async-First**: Native async/await support
- 🛠 **Resource Efficient**: Optimized connection pooling and memory usage
- 🌐 **HTTP/2 Support**: Modern protocol features out of the box

## 🚄 Performance

Real-world benchmarks show significant performance improvements over other popular HTTP clients:

| Client       | Mean (ms) | Min (ms) | Max (ms) |
|-------------|-----------|----------|-----------|
| SharpHTTP   | 13.50     | 9.41     | 26.18    |
| aiohttp     | 45.06     | 21.79    | 1062.42  |

**SharpHTTP is 233.7% faster on average!** 📈

## 🔧 Installation

```bash
pip install sharphttp
```

## 📚 Usage

```python
import asyncio
from sharphttp import ClientSession

async def main():
    async with ClientSession() as session:
        # Simple GET request
        response = await session.get('https://api.example.com/data')
        
        # Print response status
        print(response.status)  # 200
        
        # Get response text
        text = await response.text()
        print(text)

        # With headers and query parameters
        response = await session.get(
            'https://api.example.com/search',
            headers={'Authorization': 'Bearer token'},
            params={'q': 'search term'}
        )

asyncio.run(main())
```

## 🔍 API Reference

### ClientSession

The main interface for making HTTP requests.

#### Methods

- `__init__()`: Create a new client session
- `get(url, *, headers=None, params=None)`: Perform GET request
  - `url`: Target URL (string)
  - `headers`: Optional dictionary of headers
  - `params`: Optional dictionary of query parameters

### Response

Represents an HTTP response.

#### Properties

- `status`: HTTP status code (int)
- `headers`: Response headers (dict)

#### Methods

- `text()`: Get response body as text (async)

## 🏗 Building from Source

1. Install Rust and Python development dependencies
2. Clone the repository
```bash
git clone https://github.com/theoneandonlyacatto/sharphttp
cd sharphttp
```
3. Build the package
```bash
pip install maturin
maturin develop --release
```

## 🤝 Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## 📝 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## 🙏 Acknowledgments

- Built with [PyO3](https://github.com/PyO3/pyo3)
- Powered by [Hyper](https://github.com/hyperium/hyper)

---

<p align="center">Made with ❤️ by <a href="https://github.com/theoneandonlyacatto">acatto</a></p>
