# sciserver-upload-rs
A tool to upload files to sciserver concurrently written in rust.

To install:

```
cargo install https://github.com/amitschang/sciserver-upload-rs
```

Or check out this package and build or run with cargo:

```
git clone https://github.com/amitschang/sciserver-upload-rs
cargo run -- --help
```

At a minimum your sciserver token either needs to be in environment
`SCISERVER_TOKEN` or specified as option. Then pass the volume path (e.g.
`Storage/arik/persistent/test`) and any number of files to upload:

```
upload --token thetoken Storage/arik/persistent/test *.csv
```

See the help:

```
Usage: upload [OPTIONS] <PATH> [FILES]...

Arguments:
  <PATH>      path to upload files to
  [FILES]...  files to upload

Options:
  -e, --endpoint <ENDPOINT>  sciserver fileservice http endpoint, defaults to that of jhu-prod
  -t, --token <TOKEN>        sciserver token, defaults to SCISERVER_TOKEN env var
  -c, --cons <CONS>          number of concurrent uploads, defaults to 10
  -r, --retries <RETRIES>    number of retries for each upload, defaults to 3
  -f, --force                overwrite existing files, defaults to false
  -h, --help                 Print help
```
