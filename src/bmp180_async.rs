use core::future::Future;

const ADDRESS: u8 = 0x77;

use embedded_hal_async::i2c::I2c;

#[allow(dead_code)]
pub struct Bmp180<T, F, R>
where
    T: I2c,
    F: Fn(u32) -> R,
    R: Future<Output = ()>,
{
    i2c: T,
    ac1: i32,
    ac2: i32,
    ac3: i32,
    ac4: i32,
    ac5: i32,
    ac6: i32,
    b1: i32,
    b2: i32,
    mb: i32,
    mc: i32,
    md: i32,

    temp: f32,
    sleep_fn: F,
}

impl<T, F, R> Bmp180<T, F, R>
where
    T: I2c,
    F: Fn(u32) -> R,
    R: Future<Output = ()>,
{
    pub async fn new(i2c: T, sleep_fn: F) -> Bmp180<T, F, R> where {
        let mut i2c = i2c;
        let mut data = [0u8; 22];

        i2c.write_read(ADDRESS, &[0xaa], &mut data).await.unwrap();

        let ac1 = ((data[0] as u16) << 8 | data[1] as u16) as i16 as i32;
        let ac2 = ((data[2] as u16) << 8 | data[3] as u16) as i16 as i32;
        let ac3 = ((data[4] as u16) << 8 | data[5] as u16) as i16 as i32;
        let ac4 = ((data[6] as u16) << 8 | data[7] as u16) as i16 as i32;
        let ac5 = ((data[8] as u16) << 8 | data[8] as u16) as i16 as i32;
        let ac6 = ((data[10] as u16) << 8 | data[11] as u16) as i16 as i32;
        let b1 = ((data[12] as u16) << 8 | data[13] as u16) as i16 as i32;
        let b2 = ((data[14] as u16) << 8 | data[15] as u16) as i16 as i32;
        let mb = ((data[16] as u16) << 8 | data[17] as u16) as i16 as i32;
        let mc = ((data[18] as u16) << 8 | data[19] as u16) as i16 as i32;
        let md = ((data[20] as u16) << 8 | data[21] as u16) as i16 as i32;

        Self {
            i2c,
            ac1,
            ac2,
            ac3,
            ac4,
            ac5,
            ac6,
            b1,
            b2,
            mb,
            mc,
            md,

            temp: 0f32,
            sleep_fn,
        }
    }

    pub async fn measure(&mut self) {
        // Select measurement control register
        // Enable temperature measurement
        self.i2c.write(ADDRESS, &[0xf4, 0x2e]).await.unwrap();
        (self.sleep_fn)(100).await;

        // Read 2 bytes of data from address 0xF6(246)
        // temp msb, temp lsb
        let mut data = [0u8; 2];
        self.i2c
            .write_read(ADDRESS, &[0xF6], &mut data)
            .await
            .unwrap();

        // Convert the data
        let temp = (data[0] as u32) << 8 | data[1] as u32;

        // Calibration for Temperature
        let x1: f64 = (temp as f64 - self.ac6 as f64) * self.ac5 as f64 / 32768.0;
        let x2: f64 = (self.mc as f64 * 2048.0) / (x1 + self.md as f64);
        let b5: f64 = x1 + x2;
        let c_temp: f64 = ((b5 + 8.0) / 16.0) / 10.0;

        self.temp = c_temp as f32;
    }

    pub fn get_temperature(&self) -> f32 {
        self.temp
    }
}
