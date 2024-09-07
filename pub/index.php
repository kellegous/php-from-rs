<?php

declare(strict_types=1);

$method = $_SERVER['REQUEST_METHOD'] ?? 'GET';
if ($method === 'GET') {
	http_response_code(200);
	header('Content-Type: application/json');
	echo json_encode($_SERVER, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES);
	return;
}

$data = json_decode(file_get_contents('php://input'));
if (json_last_error() !== JSON_ERROR_NONE) {
	http_response_code(400);
	header('Content-Type: application/json');
	echo json_encode(['message' => 'Invalid JSON'], JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES);
	return;
}

http_response_code(200);
header('Content-Type: application/json');
echo json_encode($data, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES);