Plan:
* Build abstraction around UDP sockets (interface)
  * Enable and disable interfaces
  * `send` takes in frame
  * `recv` returns Result<frame>
  * observe MTU
  * be linient with incoming


* frame struct:
  * look at /usr/include/netinet/ip.h
  * Headers as fields
  * also has data field
  * to and from bytes methods

```
struct Interface {
  udp,
  state: Arc<(Mutex<StateEnum>, Condvar)>
}

impl Interface {
  new {channels}
  spawn_write_thread()
  spawn_read_thread()
  up {}
  down {}
}


struct Node {
 some links
 some other overhead data maybe?
}

impl Node {
  new (link stuff) {
  }
  
  register_handler(protocol_num: enum, handler) {
  }

  run {
    loop {
      read from links
      Node::handle_packet()
    }
  }

  handle_packet() {
    match protocol_num {
      RIP(200) => forward()
      _ => call handler based protocl num
    }
  }
}

udp_recv ------- LinkLayer -- node
             /
udp_send ----

struct controller {
    interface_map {
        their_ip: interface {
            state,
            our_ip,
        }
    }

    fn send

    fn recv

    fn up (interface_id)
    fn down (interface_id)
}
```
