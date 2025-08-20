# Steam Auth

A simple command-line tool to generate Steam authentication tickets for web API usage.

## Features

- Generate Steam authentication tickets using the Steamworks SDK
- Save tickets to file or POST to a web endpoint
- Cross-platform support (Windows, Linux)

## Usage

```bash
# Save ticket to file (default: auth_ticket.txt)
steam-auth

# Save to custom file
steam-auth -o my_ticket.txt

# POST ticket to web endpoint with email
steam-auth -u https://api.example.com/auth -e user@example.com

# Exit immediately after saving ticket
steam-auth -o ticket.txt -x
```

## Requirements

- Steam client must be running and logged in
- Your application must be registered as a Steam game (requires `steam_appid.txt` in working directory)

## Installation

Download the latest binary from the [releases page](https://github.com/infernap12/steam-auth/releases) or build from source:

```bash
git clone https://github.com/infernap12/steam-auth.git
cd steam-auth
cargo build --release
```

## License

GPL-3.0 License - see [LICENSE](LICENSE) file for details.
