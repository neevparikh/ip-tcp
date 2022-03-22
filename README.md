## Building

If running in the class Docker container, make sure to run this first:

```
sudo chmod o+w -R /opt/rust
```

Once that is done, just run `make` in the project root. This will build and move `node` into the
root.

## Design

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
  link layer packets. If the packet is malformed, we drop it. If we get a packet meant for us, we
  call the appropriate handler. If not, and the time to live is 0, we drop the packet. Otherwise, we
  decrement the TTL and forward the packet using the forwarding table.

## Details

* We developed on our machines and the docker container
* We saw a weird piece of behavior where on our local laptops, the reference node and our node
  couldn't send messages between each other. However, reference nodes could send to reference nodes
  and our node could send to other our nodes. 
  However, on the container, our nodes can send to the reference nodes and vice-versa. As a result,
  we decided it was okay. Curiously, Wireshark says that the packets from the reference nodes error
  because it says UDP port not available. 
* We decided to not remove entries from the routing table if they are set to an infinite cost, but
  we make sure we don't send to them.
