<?php
function is_prime($n) {
    if ($n < 2) return false;
    $d = 2;
    while ($d * $d <= $n) { if ($n % $d == 0) return false; $d += 1; }
    return true;
}
$t0 = hrtime(true);
$i = 2; $count = 0; $checksum = 0;
while ($i < 50000) { if (is_prime($i)) { $count += 1; $checksum += $i; } $i += 1; }
$us = intdiv(hrtime(true) - $t0, 1000);
echo "RESULT $count $checksum\n";
echo "TIME_US $us\n";
