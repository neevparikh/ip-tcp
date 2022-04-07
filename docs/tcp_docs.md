## Building

If running in the class Docker container, make sure to run this first:

```
sudo chmod o+w -R /opt/rust
```

Once that is done, just run `make` in the project root. This will build and move `node` into the
root.

## Design

Milestone
* clean up dir structure + tcp layer.rs
* add another binary
* get TCP header => etherparse
* TCP node repl
* make socket API -> extend interface 
* understand state machine
    - function to deal
    - switch case

Bonus:
* data structure for sliding window
