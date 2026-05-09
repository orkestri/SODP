module github.com/orkestri/sodp-go/sodpredis

go 1.21

require (
	github.com/google/uuid v1.6.0
	github.com/orkestri/sodp-go v0.0.0
	github.com/redis/go-redis/v9 v9.5.1
	github.com/vmihailenco/msgpack/v5 v5.4.1
)

require (
	github.com/cespare/xxhash/v2 v2.2.0 // indirect
	github.com/dgryski/go-rendezvous v0.0.0-20200823014737-9f7001d12a5f // indirect
	github.com/golang-jwt/jwt/v5 v5.3.0 // indirect
	github.com/gorilla/websocket v1.5.3 // indirect
	github.com/vmihailenco/tagparser/v2 v2.0.0 // indirect
)

replace github.com/orkestri/sodp-go => ../
