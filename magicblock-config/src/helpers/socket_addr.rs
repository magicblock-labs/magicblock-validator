macro_rules! socket_addr_config {
    ($struct_name:ident, $port:expr, $label:expr) => {
        #[derive(
            Debug,
            Clone,
            PartialEq,
            Eq,
            ::serde::Deserialize,
            ::serde::Serialize,
        )]
        #[serde(deny_unknown_fields)]
        pub struct $struct_name {
            #[serde(
                default = "default_addr",
                deserialize_with = "deserialize_addr",
                serialize_with = "serialize_addr"
            )]
            pub addr: ::std::net::IpAddr,
            #[serde(default = "default_port")]
            pub port: u16,
        }

        impl Default for $struct_name {
            fn default() -> Self {
                Self {
                    addr: default_addr(),
                    port: default_port(),
                }
            }
        }

        impl $struct_name {
            pub fn socket_addr(&self) -> ::std::net::SocketAddr {
                ::std::net::SocketAddr::new(self.addr, self.port)
            }
        }

        fn default_port() -> u16 {
            $port
        }

        fn deserialize_addr<'de, D>(
            deserializer: D,
        ) -> Result<::std::net::IpAddr, D::Error>
        where
            D: ::serde::Deserializer<'de>,
        {
            let s =
                <String as ::serde::Deserialize>::deserialize(deserializer)?;
            s.parse().map_err(|err| {
                // The error returned here by serde is a bit unhelpful so we help out
                // by logging a bit more information.
                eprintln!(
                    "The [{}] field 'addr' is invalid ({:?}).",
                    $label, err
                );
                ::serde::de::Error::custom(err)
            })
        }

        fn serialize_addr<S>(
            addr: &::std::net::IpAddr,
            serializer: S,
        ) -> Result<S::Ok, S::Error>
        where
            S: ::serde::Serializer,
        {
            serializer.serialize_str(addr.to_string().as_ref())
        }

        fn default_addr() -> ::std::net::IpAddr {
            ::std::net::IpAddr::V4(::std::net::Ipv4Addr::new(0, 0, 0, 0))
        }
    };
}
pub(crate) use socket_addr_config;
