@{
    MihomoVersion = 'v1.19.32'
    MihomoZip = @{
        Url = 'https://github.com/MetaCubeX/mihomo/releases/download/v1.19.32/mihomo-windows-amd64-v1.19.32.zip'
        Sha256 = '1AC84E795B5B915446E20139677FB6CC013A0778E876E8D4C96F994750B4C4A3'
    }
    MihomoExeSha256 = 'BEB9878924D7BD38C67176B441AB5E668CC378BFE751C8DC8FEF3EB5AAF566D5'
    # meta-rules-dat snapshot: 2026-10-21T00:44:55Z; update commit and hashes together.
    GeoBase = 'https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat/eed74d50aa6083d028110ed3feaf37f5e3070d9d'
    Geo = @(
        @{ Name = 'GEOIP.metadb'; Source = 'geoip.metadb'; Sha256 = 'B9C84A522DA098BC3E8A23E30E426BC3C916D532B1D7D4E4B41590D5F847DAA2' },
        @{ Name = 'GEOSITE.dat'; Source = 'geosite.dat'; Sha256 = '77A487C5599EDB9B69C9EA28BBF07385DD186156A9D7F3EBE3BFAD397F1CC2C9' },
        @{ Name = 'GEOIP.dat'; Source = 'geoip.dat'; Sha256 = '4A1B1EDDABF5614FA51DD93AC0498856CB82DD65933CD6D6D5890D8C7BB5E902' },
        @{ Name = 'ASN.mmdb'; Source = 'GeoLite2-ASN.mmdb'; Sha256 = 'B9C1120510F02791C455FA667740A251E46E85D625FB977F8248D5EC1EBF6721' }
    )
}
