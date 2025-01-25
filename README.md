# imputio
`imputio` is a scratch workspace for implementing a toy async runtime. 
It is intended for enabling experimentation with different scheduler/
executor designs, and is a vehicle for learning more about the internals of asynchronous runtimes. I am interested in the performance and complexity tradeoffs of different designs, for example:
- runtimes using kernel-backed datastructures like epoll or io_uring 
- runtimes using multithreading versus thread-per-core
- scheduling algorithms like priority queuing, batch processing, workstealing, etc. 

Lots of inspiration has been derived from `smol`, `glommio`, `tokio-uring`, and all the other runtimes out there.