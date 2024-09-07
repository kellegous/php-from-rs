<?php

declare(strict_types=1);

http_response_code(200);
header('Content-Type: application/json');
echo json_encode($_SERVER, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES);