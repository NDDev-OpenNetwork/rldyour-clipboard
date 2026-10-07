$ErrorActionPreference = 'Stop'
$socket = [System.Net.Sockets.Socket]::new([System.Net.Sockets.AddressFamily]::Unix, [System.Net.Sockets.SocketType]::Stream, [System.Net.Sockets.ProtocolType]::Unspecified)
$socket.ReceiveTimeout = 5000
$socket.SendTimeout = 5000
$socket.Connect([System.Net.Sockets.UnixDomainSocketEndPoint]::new((Join-Path $env:RLDYOUR_CLIPBOARD_HOME 'rldyour-clipboard.sock')))
$stream = [System.Net.Sockets.NetworkStream]::new($socket, $false)
$encoding = [System.Text.UTF8Encoding]::new($false)
function Send-Frame($frame) {
  $bytes = $encoding.GetBytes(($frame | ConvertTo-Json -Compress -Depth 8) + "`n")
  $stream.Write($bytes, 0, $bytes.Length)
}
function Read-Frame {
  $bytes = [System.Collections.Generic.List[byte]]::new()
  while ($true) {
    $value = $stream.ReadByte()
    if ($value -lt 0) { throw 'daemon closed early' }
    if ($value -eq 10) { break }
    $bytes.Add([byte]$value)
    if ($bytes.Count -gt 65536) { throw 'oversized control frame' }
  }
  return ($encoding.GetString($bytes.ToArray()) | ConvertFrom-Json)
}
try {
  Send-Frame @{op='hello';v=1;role='both';watch=$false}
  $hello = Read-Frame
  if ($hello.ev -ne 'hello' -or $hello.v -ne 1 -or $hello.retention_days -ne 7) { throw 'hello contract' }
  Send-Frame @{op='begin';req=1;source='synthetic-windows-test'}
  $draft = (Read-Frame).draft
  $content = $encoding.GetBytes('Synthetic Windows clipboard protocol test')
  Send-Frame @{op='part';req=2;draft=$draft;mime='text/plain';bytes=$content.Length}
  $stream.Write($content,0,$content.Length)
  if ((Read-Frame).ev -ne 'ok') { throw 'part' }
  Send-Frame @{op='commit';req=3;draft=$draft}
  $entry = (Read-Frame).entry
  Send-Frame @{op='pin';req=4;entry=$entry;pinned=$true}
  if ((Read-Frame).ev -ne 'ok') { throw 'pin' }
  Send-Frame @{op='clear';req=5}
  if ((Read-Frame).ev -ne 'ok') { throw 'clear' }
  Send-Frame @{op='list';req=6;pinned=$true}
  $items = @((Read-Frame).items)
  if ($items.Count -ne 1 -or $items[0].id -ne $entry -or !$items[0].pinned) { throw 'durable pin' }
  Send-Frame @{op='fetch';req=7;entry=$entry;mime='text/plain'}
  $blob = Read-Frame
  $payload = [byte[]]::new([int]$blob.bytes)
  $offset = 0
  while ($offset -lt $payload.Length) {
    $n = $stream.Read($payload,$offset,$payload.Length-$offset)
    if ($n -eq 0) { throw 'truncated payload' }; $offset += $n
  }
  if ($encoding.GetString($payload) -ne $encoding.GetString($content)) { throw 'restored bytes' }
  Write-Output 'PASS: native Windows socket, capture protocol, pin survives clear, exact restore'
} finally { $stream.Dispose(); $socket.Dispose() }
