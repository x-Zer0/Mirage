@{
    MihomoVersion = 'v1.19.32'
    MihomoZip = @{
        Url = 'https://github.com/MetaCubeX/mihomo/releases/download/v1.19.32/mihomo-windows-amd64-v1.19.32.zip'
        Sha256 = '1AC84E795B5B915446E20139677FB6CC013A0778E876E8D4C96F994750B4C4A3'
    }
    MihomoExeSha256 = 'BEB9878924D7BD38C67176B441AB5E668CC378BFE751C8DC8FEF3EB5AAF566D5'
    # meta-rules-dat publication: 2026-10-15T00:42:34Z; fail if rolling release changes.
    GeoBase = 'https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest'
    Geo = @(
        @{ Name = 'GEOIP.metadb'; Source = 'geoip.metadb'; Sha256 = '262BC4A119A9A3510AE957E434E11BFA41C37D72845BDA9DBEA12C3F2AC28B36' },
        @{ Name = 'GEOSITE.dat'; Source = 'geosite.dat'; Sha256 = '9AE9D588B1C051C764F2500E188393934AA84FC45E0DA96530DDF069CF232B27' },
        @{ Name = 'GEOIP.dat'; Source = 'geoip.dat'; Sha256 = '1ADAEEC24369B866B3398E8D46992F25293FDD7A7BF78135558BD767AC264D8B' },
        @{ Name = 'ASN.mmdb'; Source = 'GeoLite2-ASN.mmdb'; Sha256 = 'D82D2F39EA82A175A3D63484C8B4723040D134BA1EDF4B27D6819C2F96C14A9B' }
    )
}
