use dns_lookup::lookup_host;
use std::net::SocketAddr;

pub const DNS_SEED_SERVERS: [&str; 9] = [
    "seed.bitcoin.sipa.be",
    "dnsseed.bluematt.me.",
    "dnsseed.bitcoin.dashjr-list-of-p2p-nodes.us.",
    "seed.bitcoin.jonasschnelli.ch.",
    "seed.btc.petertodd.net.",
    "seed.bitcoin.sprovoost.nl.",
    "dnsseed.emzy.de.",
    "seed.bitcoin.wiz.biz.",
    "seed.mainnet.achownodes.xyz.",
];

pub fn fetch_bitcoin_peers(dns_seed: &str) -> Result<Vec<SocketAddr>, String> {
    match lookup_host(dns_seed) {
        Ok(ips) => {
            let peers: Vec<SocketAddr> = ips
                .into_iter()
                .map(|ip| SocketAddr::new(ip, 8333))
                .collect();
            Ok(peers)
        }
        Err(e) => Err(format!("DNS lookup failed: {}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fetch_peers() {
        let peers = fetch_bitcoin_peers("seed.bitcoin.sipa.be").unwrap();
        println!("{} peers:", peers.len());
        for peer in peers {
            println!("{peer}");
        }
    }
}
