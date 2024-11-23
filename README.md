# sharphttp
a super-fast easy-to-use HTTP client, for python written in Rust

Results over 100 requests:
Client          Mean (ms)    Min (ms)     Max (ms)
---------------------------------------------------
Sharp Client          13.50        9.41       26.18
aiohttp               45.06       21.79     1062.42

Sharp client is 233.7% faster on average
