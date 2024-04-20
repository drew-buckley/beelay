# beelay
`beelay` is a simple webserver that provides a HTTP API for control of Zigbee smart switches. This is expected to be run in concert with `zigbee2mqtt` and sit on the edge of the MQTT network.

The motivation of this program was to expose a simple control interface to my MQTT network for my Zigbee switches, but without exposing the MQTT network to the outside network. 

I personally have been running this on a Raspberry Pi 3 and controlling SONOFF Zigbee switches. Support for other setups is largely untested.

There's also a frontend now! Its hacky as hellll, and it lags, and it is just generally bad (I'm an embedded dev, not a frontend one). But it does work in a pinch. I'll improve it eventually, but I primarily use an Android app I wrote to control thing, so it may take me a while to get to.

## API Usage
```
~$ curl  --request POST "pi.local:9999/api/switch/Living%20Room%20Lights/?state=on" ; echo
{"status":"success"}
```

```
~$ curl  --request GET "pi.local:9999/api/switch/Living%20Room%20Lights" ; echo
{"status":"success","state":"on"}
```

```
~$ curl  --request POST "pi.local:9999/api/switch/Living%20Room%20Lights/?state=off" ; echo
{"status":"success"}
```

```
~$ curl  --request GET "pi.local:9999/api/switch/Living%20Room%20Lights" ; echo
{"status":"success","state":"off"}
```

Example of an error:
```
~$ curl  --request GET "pi.local:9999/api/?switch=Not%20a%20real%20device" ; echo
{"error_msg":"Unknown switch name.","status":"error"}
```

## Frontend Client Usage
[http://pi.local:9999/client](http://pi.local:9999/client)

## Configuration Example
### beelay.toml
```
[service]
bind_address = "0.0.0.0"
port = 9999
switches="Bedroom TV,Living Room Lights,Bedroom Lights,Office Lights,XMass Tree"
cache_dir = "/run/beelay"

[mqttbroker]
host = "192.168.1.2"
port = 1883
topic = "zigbee2mqtt"
```

### systemd
```
[Unit]
After=network.target
Description=Beelay HTTP to MQTT Bridge

[Service]
Type=notify
ExecStart=beelay --syslog --sd-notify -c /etc/beelay/beelay.toml
Restart=always
WatchdogSec=20

[Install]
WantedBy=multi-user.target
```
