# EkaCI Server

This is intended to be the running EkaCI service

## Structure

The project is structured as such:

```
./client       # The cli which allows users to interact with a locally running instance over a unix socket
./evaluator    # Logic around querying derivations, traversing their graph, and communicating that to other eka services
./server       # Web server and handles events. Currently also handles PR checkout, nix builds, and database CRUD operations
./shared       # Mostly shared types and functions which are useful across 2 or more services
```
