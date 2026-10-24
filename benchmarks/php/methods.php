<?php
class Counter {
    public int $n = 0;
    public function bump(): int {
        $this->n = $this->n + 1;
        return $this->n;
    }
}

function run() {
    $n = 200000;
    $c = new Counter();
    $i = 0;
    $last = 0;
    while ($i < $n) {
        $last = $c->bump();
        $i = $i + 1;
    }
    echo $last . PHP_EOL;
}
run();
