## general

1. consider master mode in this round

## config

```
[pins.i2c]
"I2C_1" = { device = "/dev/i2c-1", sda = 10, scl = 8 }
```

- `"I2C_1"`: registered i2c bus name
- `device`: corresponding i2c device
- `sda` & `scl`: pin index of i2c SDA and SCL. for example, in [gpio.json](../assets/rock5b/gpio.json), pin `8` have a function `I2C1_SCL_M0`, pin `10` have a function `I2C1_SDA_M0`, means I2C1 (`/dev/i2c-1`) requires SCL=pin`8`, SDA=pin`10`

## request

### query

1. `target = "i2c"` list all i2c bus. like `i2cdetect -l`, returns registered name; if it have ONE parameter `name` as registered i2c name, return support functionalities like `i2cdetect -F <N>`

2. `target = "i2c:scan"`, take required ONE parameter `name` as registered i2c bus name. scan specific i2c bus to find exsit device; like `i2cdetect -y <N>`

### init

add `mode = "i2c"`

currently no other fields need to support. (setting frequency is not support by all devices)

NOTICE: `address` should be dynamic, and not fixed by init.

### [NEW] read / write / fetch

after init;

take required ONE parameter `address` as target device address. 

take parameter `register` to specific register. 

- `read`: read from address device.

- `write`: write to address device (without response).

- `fetch`:  write to address device, and wait for data response

parameter `format`: "raw" (directly use json string without string validation), "hex", "base64", "bytes" (json array of bytes); all serialize/deserialize to wrapped `[u8]`

`read` / `write` / `fetch` action may also be reused in other transfer interface, for example, spi. reserved it.

