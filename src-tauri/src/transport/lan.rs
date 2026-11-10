//! 局域网 UDP 传输（架构 6.3）。只收发字节，不知道房间密钥。
//!
//! 实现流程：
//! 1. 一个 UDP socket，默认绑定 `0.0.0.0:47480`，开启广播。第一次运行时 Windows 防火墙会弹窗，需要允许「专用网络」。
//! 2. `broadcast`：发往 255.255.255.255，以及每块「已启用、非回环、非点对点、有广播地址」网卡的定向广播地址。
//!    这台机器开着 Clash TUN（点对点网卡）和 VMware 虚拟网卡，只发 255.255.255.255 可能走错网卡。
//!    网卡列表最多 5 秒查一次（3.2.0：以前每次广播都现查，在线状态每 2 秒、说话时每秒好几个包，查网卡挺费）；
//!    换了网络最多 5 秒后跟上，和节点查「网络变了没」的间隔一样。
//! 3. 接收循环在后台任务里：每个包先过洪水防护，再连同来源地址送进通道，交给节点处理；
//!    节点停止（通道接收端被丢弃）时接收循环随之退出，释放端口。
//!    Windows 上发往无人监听的端口后，下一次接收会报「连接被重置」，这类错误直接忽略、继续接收。
//! 4. 测试和单机双开时，可以绑定 127.0.0.1，并用固定的对端列表代替广播。

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use super::flood::FloodGuard;
use crate::protocol::packet::MAX_PACKET_LEN;

pub const LAN_PORT: u16 = 47480;
/// 算好的广播地址用多久再重新查网卡（文件头第 2 条）。
const TARGETS_FRESH: Duration = Duration::from_secs(5);

/// 收到的包：(来源地址, 字节)。
pub type Inbox = mpsc::Receiver<(SocketAddr, Vec<u8>)>;

#[derive(Clone, Debug)]
pub struct LanConfig {
    pub bind: SocketAddr,
    /// 广播发往这些端口（单机双开时两个实例的端口都要发）。
    pub broadcast_ports: Vec<u16>,
    /// 设置后不再广播，改为逐个单播到这些地址（测试用）。
    pub fixed_targets: Option<Vec<SocketAddr>>,
    /// 洪水防护：(每 IP 每秒, 总计每秒, 最多跟踪地址数)。
    pub flood: (u64, u64, usize),
}

impl Default for LanConfig {
    fn default() -> Self {
        LanConfig {
            bind: SocketAddr::from(([0, 0, 0, 0], LAN_PORT)),
            broadcast_ports: vec![LAN_PORT],
            fixed_targets: None,
            flood: (100, 800, 1024),
        }
    }
}

/// 本次广播要发往的地址。
/// 本机已启用、非回环网卡的地址，排好序。节点定时比较它来发现「网络变了」（比如连上了别的 Wi-Fi）。
pub fn interface_signature() -> Vec<std::net::IpAddr> {
    let mut ips: Vec<std::net::IpAddr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback() && i.is_oper_up())
        .map(|i| i.ip())
        .collect();
    ips.sort();
    ips
}

pub fn broadcast_targets(cfg: &LanConfig) -> Vec<SocketAddr> {
    if let Some(fixed) = &cfg.fixed_targets {
        return fixed.clone();
    }
    let mut ips = vec![Ipv4Addr::BROADCAST];
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for i in ifaces {
            if i.is_loopback() || i.is_p2p() || !i.is_oper_up() {
                continue;
            }
            if let if_addrs::IfAddr::V4(v4) = &i.addr
                && let Some(b) = v4.broadcast
                && !ips.contains(&b)
            {
                ips.push(b);
            }
        }
    }
    ips.iter()
        .flat_map(|ip| {
            cfg.broadcast_ports
                .iter()
                .map(move |p| SocketAddr::from((*ip, *p)))
        })
        .collect()
}

pub struct LanTransport {
    socket: Arc<UdpSocket>,
    cfg: LanConfig,
    /// 上次算出的广播地址和算的时间。
    targets: Mutex<Option<(Instant, Vec<SocketAddr>)>>,
}

impl LanTransport {
    pub async fn bind(cfg: &LanConfig) -> std::io::Result<(Self, Inbox)> {
        let socket = Arc::new(UdpSocket::bind(cfg.bind).await?);
        socket.set_broadcast(true)?;
        let (tx, rx) = mpsc::channel(1024);
        let (per_ip, total, tracked) = cfg.flood;
        let mut guard = FloodGuard::new(per_ip, total, tracked);
        let recv_socket = socket.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            let mut buf = vec![0u8; 2048];
            loop {
                // 节点停止（通道接收端被丢弃）时立刻退出，释放 socket，端口才能被重新绑定
                let received = tokio::select! {
                    _ = tx.closed() => break,
                    r = recv_socket.recv_from(&mut buf) => r,
                };
                let Ok((n, from)) = received else {
                    continue; // 包括 Windows 的「连接被重置」：忽略，继续收
                };
                if !guard.allow(from.ip(), started.elapsed().as_millis() as u64) {
                    continue;
                }
                // 我们的包最多 1200 字节，更长的不是我们的：不复制、不交给节点去解密（3.3.0）
                if n > MAX_PACKET_LEN {
                    continue;
                }
                if tx.send((from, buf[..n].to_vec())).await.is_err() {
                    break; // 节点已经停了
                }
            }
        });
        Ok((
            LanTransport {
                socket,
                cfg: cfg.clone(),
                targets: Mutex::new(None),
            },
            rx,
        ))
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.socket
            .local_addr()
            .expect("已绑定的 socket 一定有本地地址")
    }

    /// 发往所有广播地址。单个地址发送失败（比如某块网卡刚断开）不影响其他地址。
    pub async fn broadcast(&self, bytes: &[u8]) {
        for t in self.targets() {
            let _ = self.socket.send_to(bytes, t).await;
        }
    }

    /// 广播地址：5 秒内算过就直接用，过了再查一次网卡。
    fn targets(&self) -> Vec<SocketAddr> {
        let mut cache = self.targets.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, list)) = cache.as_ref()
            && at.elapsed() < TARGETS_FRESH
        {
            return list.clone();
        }
        let list = broadcast_targets(&self.cfg);
        *cache = Some((Instant::now(), list.clone()));
        list
    }

    pub async fn send_to(&self, addr: SocketAddr, bytes: &[u8]) {
        let _ = self.socket.send_to(bytes, addr).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn loopback(peers: Vec<SocketAddr>) -> LanConfig {
        LanConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            fixed_targets: Some(peers),
            ..LanConfig::default()
        }
    }

    async fn recv(rx: &mut Inbox) -> Option<(SocketAddr, Vec<u8>)> {
        tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .ok()
            .flatten()
    }

    #[tokio::test]
    async fn broadcast_and_unicast_between_two_sockets() {
        let (b, mut b_rx) = LanTransport::bind(&loopback(vec![])).await.unwrap();
        let (a, _a_rx) = LanTransport::bind(&loopback(vec![b.local_addr()]))
            .await
            .unwrap();
        a.broadcast(b"hello").await;
        let (from, bytes) = recv(&mut b_rx).await.expect("B 应收到广播");
        assert_eq!(
            (from, bytes.as_slice()),
            (a.local_addr(), b"hello".as_slice())
        );
        a.send_to(b.local_addr(), b"direct").await;
        assert_eq!(recv(&mut b_rx).await.unwrap().1, b"direct".to_vec());
    }

    #[tokio::test]
    async fn packets_longer_than_ours_are_dropped_before_the_node_sees_them() {
        let (b, mut b_rx) = LanTransport::bind(&loopback(vec![])).await.unwrap();
        let (a, _a_rx) = LanTransport::bind(&loopback(vec![])).await.unwrap();
        a.send_to(b.local_addr(), &vec![7u8; MAX_PACKET_LEN + 1])
            .await;
        a.send_to(b.local_addr(), &vec![8u8; MAX_PACKET_LEN]).await;
        // 超长的那个先发：收到的第一个就是 1200 字节的，说明 1201 字节的没进通道
        let (_, bytes) = recv(&mut b_rx).await.expect("1200 字节的包照收");
        assert_eq!(bytes, vec![8u8; MAX_PACKET_LEN]);
        assert!(recv(&mut b_rx).await.is_none());
    }

    #[tokio::test]
    async fn sending_to_a_closed_port_does_not_kill_the_receiver() {
        // Windows 上，发往没人监听的端口后，下一次接收会报「连接被重置」，接收循环必须继续
        let (a, mut a_rx) = LanTransport::bind(&loopback(vec![])).await.unwrap();
        let closed = {
            let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            s.local_addr().unwrap()
        };
        a.send_to(closed, b"nobody").await;
        let (b, _b_rx) = LanTransport::bind(&loopback(vec![])).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        b.send_to(a.local_addr(), b"still alive").await;
        assert_eq!(recv(&mut a_rx).await.unwrap().1, b"still alive".to_vec());
    }

    #[tokio::test]
    async fn flood_guard_drops_excess_packets() {
        let mut cfg = loopback(vec![]);
        cfg.flood = (3, 800, 1024);
        let (b, mut b_rx) = LanTransport::bind(&cfg).await.unwrap();
        let (a, _a_rx) = LanTransport::bind(&loopback(vec![])).await.unwrap();
        for _ in 0..10 {
            a.send_to(b.local_addr(), b"spam").await;
        }
        let mut got = 0;
        while recv(&mut b_rx).await.is_some() {
            got += 1;
        }
        assert_eq!(got, 3);
    }

    #[test]
    fn default_targets_use_broadcast_addresses_not_loopback() {
        let targets = broadcast_targets(&LanConfig::default());
        assert!(targets.contains(&SocketAddr::from(([255, 255, 255, 255], LAN_PORT))));
        assert!(targets.iter().all(|t| !t.ip().is_loopback()));
        assert!(targets.iter().all(|t| t.port() == LAN_PORT));
    }
}
