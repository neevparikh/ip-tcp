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
* The send buffer has two main components, the window, and the data. The application writes data
  into the internal buffer (blocking if the buffer is currently full), and the window keeps track of
  which portions of the buffer are currently in flight. The send buffer additionally owns two
  threads: The send thread periodically wakes up (due to new data, acks, timeouts, zero window
  probing, etc.) and is in charge of sending out retries, segmenting the data into packets, and
  forwarding packets to the tcp layer to be sent out. The timeout thread is in charge of waking the
  send thread up when timeouts have expired. The final important thing to note about the send buffer
  is that it tracks it's own state which is similar to the TCP stream state but has some difference.
  This allows sends to be queued up until the syn has been acked, and the fin can be queued until
  the current data is segmented and sent out.
* The recv buffer is backed by a underlying ring buffer and a data structure that keeps track of
  intervals of sequence numbers. It correctly handles wrapping of sequence numbers, and keeping
  track of reads and acking data. Interval tracking is handled by two binary trees that keep track
  of starts of intervals and ends of intervals. Upon adding new intervals (on receiving data from
  the stream), the existing intervals are coalesced to keep track of the minimal set of intervals.
  Intervals are pruned as the sliding window moves forward.  
* For the public facing API, we expose a TcpStream and a TcpListener, which matches the default
  language API. Listener exposes a bind and accept function, and Stream exposes a connect function.

### Congestion Control

Although we are not taking networks as a capstone course, we decided to implement congestion control
for fun (and hopefully some extra credit). We implemented the reno algorithm with slow start, and
fast recovery on repeated duplicate acks. Since we weren't implementing this as a requirement our
api is slightly different from the one specified in the handout. The simplest way to test congestion
control is to add the word `reno` as an additional argument to the send file command. Additionally,
`reno` can also be added to any connect command to force that socket to use congestion control.

### Performance

All tests involved sending a 1MB file of random bits across the network (unless otherwise specified.
Our implementation achieves comparable performance on a non-lossy network, and on a lossy network
our implementation is 11.5 times faster without congestion control, and 21.7 times faster with
congestion control. Note that the timing in very high variance since the initial timeout is very
long, so if packets are dropped early (before the RTT is learned) there will be a longer delay
before they are resent. The worst case for this is if the SYN or SYN is dropped in which our
implementation sometimes takes >3 seconds to complete.

#### B -> A (w/o lossy node)
the reference implementation.
```
            our recv | ref recv
---------------------------------------------
our send  | 108ms    | 69ms 
ref send  | 103ms    | 92ms     
```

#### C -> A with B as lossy node with 2% drop
Sending a 1MB file without a lossy node showed comparable performance across our implementation and
the reference implementation.
```
            our recv | ref recv
---------------------------------------------
our send  | 696ms    | 695ms
ref send  | 8606ms   | 8003ms
```

#### C -> A with B as lossy node with 2% drop with congestion control
```
            our recv | ref recv
---------------------------------------------
our send  | 368ms    | 449ms
ref send  | N/A      | N/A
```

#### C -> A with B as lossy node with 2% drop with congestion control 100MB file
```
            our recv | ref recv
---------------------------------------------
our send  | 16.5 sec | N/A
ref send  | N/A      | 321 sec (5.3 minutes)
```

### Wireshark Captures

#### C -> A with B as lossy node with 2% drop (file: `C_to_A_lossy.pcapng`)

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

#### C -> A with B as lossy node with 2% drop w/ congestion control (file: `C_to_A_congestion.pcapng`)

Fast Retransmission can be seen in frame 423. Additionally, looking at the start of the capture we
can see that the sent packets are sent out in larger and larger chunks until a drop occurs.

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
