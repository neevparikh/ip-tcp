## Building

If running in the class Docker container, make sure to run this first:

```
sudo chmod o+w -R /opt/rust
```

Once that is done, just run `make` in the project root. This will build and move `node` into the
root.

## TCP 

### Design 
Our implementation considers the following:

* We build upon our IP abstraction by creating a new layer struct, `tcp_layer` that hosts the TCP
  layer information. This holds the stream map, which is the underlying data structure that keeps
  track of socket ID's and streams. Our stream is represented by a TcpStream, which has two parts,
  an external struct, which holds a reference to the internal struct. The internal struct contains
  information such as the state machine state, among other pieces of synchronized information.
* TcpStream objects have two main threads that handle their state/operation. There is a send thread,
  which is responsible for sending packets out, either acks or other packets. There is a listen
  thread, that is responsible for maintaining the stream state and handling closing/etc. correctly. 
* Each stream also has access to a send and recv buffer, which are designed to correctly implement
  the sliding window algorithm. 
* TODO: write about the send buffer
* The recv buffer is backed by a underlying ring buffer and a data structure that keeps track of
  intervals of sequence numbers. It correctly handles wrapping of sequence numbers, and keeping
  track of reads and acking data. Interval tracking is handled by two binary trees that keep track
  of starts of intervals and ends of intervals. Upon adding new intervals (on receiving data from
  the stream), the existing intervals are coalesced to keep track of the minimal set of intervals.
  Intervals are pruned as the sliding window moves forward.  
* For the public facing API, we expose a TcpStream and a TcpListener, which matches the default
  language API. Listener exposes a bind and accept function, and Stream exposes a connect function.

### Wireshark Captures

#### C -> A with B as lossy node with 2% drop

Handshake

```
216 - Syn     seq = 0 | ...     | win = 65535
217 - Syn/Ack seq = 0 | ack = 1 | win = 65535
221 - Ack     seq = 1 | ack = 1 | win = 65535
```

Example segment

```
247 -  ...    seq = 6001 | ack = 1    | win = 65535 
251 -  ...    seq = 1    | ack = 6001 | win = 64535 
```

Retransmission

```
279 -  ...    seq = 15001 | ack = 1 | win = 65535
283 -  ...    seq = 15001 | ack = 1 | win = 65535
```

Teardown

```
4660 - Fin    seq = 1048577 | ...           | win = 65535
4661 - Ack    seq = 1       | ack = 1048578 | win = 65535
4670 - Fin    seq = 1       | ...           | win = 65535
4661 - Ack    seq = 1048578 | ack = 2       | win = 65535

```

## IP

### Design

Our abstraction is as follows:

* There is a link layer struct that represents the underlying link layer. The UDP socket is owned
  here and encapsulated so no other struct can directly access it. The layer exposes two channels,
  one for sending and one for receiving, and methods to examine and change states for interfaces. 
* There is a IP layer struct that has access to the link layer via the above API. It also exposes a
  send channel, but no receive channel. Instead, it allows upper layers to register a handler, that
  takes in an IP packet.
* For RIP, we have a forwarding table that maintains a map from IP addrs to a next hop interface and
  a cost. Our thread model consists of one thread that sends keep alive period messages in a loop to
  all the nodes in the network, consistent with split horizon and poison reverse. Another thread is
  a reaper thread, which checks for expired entries in the routing table and changes for the state
  of interfaces (up/down), sending triggered updates on changes, consistent with split horizon and
  poison reverse. We return a closure that is registered as a handler for when RIP packets are
  received by the IP layer. This closure responds to requests, and sends triggered updates on
  receiving responses from other nodes, as well as updating the table appropriately. We also send
  requests to neighbor nodes on initialization. 
* In processing incoming packets, we first listen on the link layer channel, which sends received
  link layer packets (at this point it is still bytes). If the packet is malformed, we drop it. If
  we get a packet meant for us, we call the appropriate handler. If not, and the time to live is 0,
  we drop the packet. Otherwise, we decrement the TTL and forward the packet using the forwarding
  table.

### Details

* We developed on our machines and the docker container
* We decided to not remove entries from the routing table if they are set to an infinite cost, but
  we make sure we don't send to them.
