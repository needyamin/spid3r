/**
 * Spid3r Mini E-Commerce Platform - Frontend Application Logic
 * Built with jQuery 3.7.1 and Bootstrap 5.3.3.
 * Seamless dual-mode: interacts with live Spid3r SQLite server OR local mock fallback.
 */

// Embedded SVG Tech Illustrations
var PRODUCT_SVGS = {
  keyboard: '<svg viewBox="0 0 120 70" fill="none" xmlns="http://www.w3.org/2000/svg"><rect x="5" y="10" width="110" height="50" rx="8" fill="#1e293b" stroke="#38bdf8" stroke-width="2"/><rect x="12" y="17" width="12" height="10" rx="2" fill="#334155"/><rect x="28" y="17" width="12" height="10" rx="2" fill="#334155"/><rect x="44" y="17" width="12" height="10" rx="2" fill="#38bdf8"/><rect x="60" y="17" width="12" height="10" rx="2" fill="#334155"/><rect x="76" y="17" width="12" height="10" rx="2" fill="#334155"/><rect x="92" y="17" width="16" height="10" rx="2" fill="#818cf8"/><rect x="12" y="31" width="14" height="10" rx="2" fill="#334155"/><rect x="30" y="31" width="12" height="10" rx="2" fill="#334155"/><rect x="46" y="31" width="12" height="10" rx="2" fill="#334155"/><rect x="62" y="31" width="12" height="10" rx="2" fill="#38bdf8"/><rect x="78" y="31" width="12" height="10" rx="2" fill="#334155"/><rect x="94" y="31" width="14" height="10" rx="2" fill="#334155"/><rect x="32" y="45" width="56" height="9" rx="2" fill="#34d399"/></svg>',
  mouse: '<svg viewBox="0 0 80 100" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M40 10C24 10 15 26 15 45C15 68 24 90 40 90C56 90 65 68 65 45C65 26 56 10 40 10Z" fill="#1e293b" stroke="#38bdf8" stroke-width="2"/><line x1="40" y1="10" x2="40" y2="40" stroke="#38bdf8" stroke-width="2"/><rect x="37" y="22" width="6" height="14" rx="3" fill="#818cf8"/><path d="M25 60C25 55 55 55 55 60" stroke="#34d399" stroke-width="1.5" stroke-linecap="round"/></svg>',
  monitor: '<svg viewBox="0 0 130 90" fill="none" xmlns="http://www.w3.org/2000/svg"><rect x="5" y="5" width="120" height="65" rx="6" fill="#1e293b" stroke="#38bdf8" stroke-width="2"/><rect x="10" y="10" width="110" height="55" rx="3" fill="#0f172a"/><path d="M25 50L50 30L75 45L105 20" stroke="#34d399" stroke-width="2" stroke-linecap="round"/><path d="M55 70L50 85H80L75 70" fill="#334155" stroke="#38bdf8" stroke-width="1.5"/></svg>',
  headphones: '<svg viewBox="0 0 100 100" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M20 50C20 30 35 15 50 15C65 15 80 30 80 50" stroke="#818cf8" stroke-width="5" stroke-linecap="round"/><rect x="14" y="48" width="14" height="30" rx="7" fill="#1e293b" stroke="#38bdf8" stroke-width="2"/><rect x="72" y="48" width="14" height="30" rx="7" fill="#1e293b" stroke="#38bdf8" stroke-width="2"/><circle cx="50" cy="50" r="14" fill="#38bdf8" fill-opacity="0.2"/></svg>',
  dock: '<svg viewBox="0 0 120 70" fill="none" xmlns="http://www.w3.org/2000/svg"><rect x="15" y="20" width="90" height="30" rx="6" fill="#334155" stroke="#38bdf8" stroke-width="2"/><rect x="25" y="32" width="10" height="6" rx="1" fill="#38bdf8"/><rect x="42" y="32" width="10" height="6" rx="1" fill="#38bdf8"/><circle cx="65" cy="35" r="3" fill="#34d399"/><rect x="76" y="30" width="18" height="10" rx="2" fill="#1e293b"/></svg>',
  mic: '<svg viewBox="0 0 80 110" fill="none" xmlns="http://www.w3.org/2000/svg"><rect x="28" y="10" width="24" height="45" rx="12" fill="#1e293b" stroke="#38bdf8" stroke-width="2"/><line x1="28" y1="25" x2="52" y2="25" stroke="#38bdf8" stroke-width="1"/><line x1="28" y1="35" x2="52" y2="35" stroke="#38bdf8" stroke-width="1"/><path d="M20 40C20 54 30 65 40 65C50 65 60 54 60 40" stroke="#818cf8" stroke-width="2" stroke-linecap="round"/><line x1="40" y1="65" x2="40" y2="85" stroke="#818cf8" stroke-width="2"/><rect x="25" y="85" width="30" height="6" rx="3" fill="#334155"/></svg>',
  mug: '<svg viewBox="0 0 90 90" fill="none" xmlns="http://www.w3.org/2000/svg"><rect x="15" y="20" width="45" height="55" rx="6" fill="#1e293b" stroke="#38bdf8" stroke-width="2"/><path d="M60 30C72 30 72 55 60 55" stroke="#38bdf8" stroke-width="3" stroke-linecap="round"/><path d="M28 12C28 8 32 8 32 4M37 14C37 10 41 10 41 6" stroke="#34d399" stroke-width="2" stroke-linecap="round"/><circle cx="37" cy="48" r="8" fill="#818cf8"/></svg>',
  mat: '<svg viewBox="0 0 120 70" fill="none" xmlns="http://www.w3.org/2000/svg"><rect x="5" y="15" width="110" height="40" rx="5" fill="#1e293b" stroke="#818cf8" stroke-width="1.5" stroke-dasharray="3 3"/><rect x="15" y="25" width="40" height="20" rx="3" fill="#334155"/><rect x="75" y="22" width="18" height="25" rx="8" fill="#38bdf8" fill-opacity="0.3"/></svg>'
};

// Baseline catalog data
var BASELINE_PRODUCTS = [
  { id: 1, name: "Spid3r Mechanical Keyboard", price: 89.99, stock: 15, category: "Peripherals", rating: 4.9, image: "keyboard", description: "Tactile hot-swappable mechanical switches, RGB backlight & wireless Bluetooth." },
  { id: 2, name: "Ergonomic Gaming Mouse", price: 49.50, stock: 20, category: "Peripherals", rating: 4.8, image: "mouse", description: "Ultra-lightweight 58g chassis, 26K optical sensor & PTFE glide feet." },
  { id: 3, name: "UltraWide 34' Curved Monitor", price: 399.00, stock: 5, category: "Displays", rating: 4.9, image: "monitor", description: "165Hz QHD 1500R curved gaming panel with HDR400 and USB-C connectivity." },
  { id: 4, name: "Spatial Audio ANC Headphones", price: 129.99, stock: 8, category: "Audio", rating: 4.7, image: "headphones", description: "Active noise cancellation, 40-hour battery life and lossless audio codec." },
  { id: 5, name: "Aluminum 10-in-1 USB-C Dock", price: 64.00, stock: 12, category: "Accessories", rating: 4.6, image: "dock", description: "Dual 4K HDMI, Gigabit Ethernet, 100W Power Delivery and SD card slots." },
  { id: 6, name: "Studio Condenser Microphone", price: 79.99, stock: 10, category: "Audio", rating: 4.8, image: "mic", description: "Cardioid polar pattern, 24-bit/192kHz sampling, built-in shock mount and pop filter." },
  { id: 7, name: "Spid3r Mascot Ceramic Mug", price: 14.99, stock: 50, category: "Merchandise", rating: 5.0, image: "mug", description: "Premium 15oz matte ceramic coffee mug featuring the official Spid3r logo." },
  { id: 8, name: "XL Precision Felt Desk Mat", price: 24.50, stock: 25, category: "Accessories", rating: 4.7, image: "mat", description: "900x400mm water-resistant felt desk protector with stitched anti-fray edges." }
];

// Application state
var isServerLive = false;
var products = $.extend(true, [], BASELINE_PRODUCTS);
var cart = { items: [], total_qty: 0, subtotal: 0, discount: 0, shipping: 0, tax: 0, total: 0, coupon: "SPID3R20" };
var orderHistory = [];
var currentCategory = 'All';
var searchQuery = '';

// ========================================================================
// jQuery Document Ready
// ========================================================================
$(function() {
  initApp();
  bindEvents();
});

function initApp() {
  checkServerHealth(function() {
    fetchProducts();
    fetchCart();
  });
}

// Server Health Check via jQuery
function checkServerHealth(callback) {
  $.ajax({
    url: '/api/health',
    type: 'GET',
    dataType: 'json',
    timeout: 2500,
    success: function(data) {
      isServerLive = true;
      $('#serverStatusBadge')
        .attr('class', 'badge bg-success-subtle text-success border border-success py-2 px-3 d-flex align-items-center gap-2');
      $('#serverStatusText').text('Spid3r SQLite Live');
      if (callback) callback();
    },
    error: function() {
      isServerLive = false;
      $('#serverStatusBadge')
        .attr('class', 'badge bg-warning-subtle text-warning border border-warning py-2 px-3 d-flex align-items-center gap-2');
      $('#serverStatusText').text('Interactive Local Mode');
      if (callback) callback();
    }
  });
}

// Fetch Products via jQuery
function fetchProducts() {
  if (isServerLive) {
    $.getJSON('/api/products', function(data) {
      if (data && data.products) {
        products = data.products;
        renderProducts();
      }
    }).fail(function() {
      renderProducts();
    });
  } else {
    renderProducts();
  }
}

// Fetch Cart via jQuery
function fetchCart() {
  if (isServerLive) {
    $.getJSON('/api/cart', function(data) {
      if (data) {
        cart = data;
        updateCartUI();
      }
    }).fail(function() {
      recalculateLocalCart();
    });
  } else {
    recalculateLocalCart();
  }
}

// Render Product Cards with jQuery
function renderProducts() {
  var $grid = $('#productsGrid');
  var filtered = $.grep(products, function(p) {
    var matchCategory = currentCategory === 'All' || p.category === currentCategory;
    var matchSearch = p.name.toLowerCase().indexOf(searchQuery.toLowerCase()) !== -1 ||
                      p.description.toLowerCase().indexOf(searchQuery.toLowerCase()) !== -1;
    return matchCategory && matchSearch;
  });

  if (filtered.length === 0) {
    $grid.html([
      '<div class="col-12 text-center py-5">',
        '<i class="bi bi-search display-5 text-secondary"></i>',
        '<h5 class="mt-3 text-secondary">No products match your criteria.</h5>',
        '<button class="btn btn-outline-info btn-sm mt-2 btn-clear-filters">Clear Filters</button>',
      '</div>'
    ].join(''));
    return;
  }

  var html = $.map(filtered, function(p) {
    var stockBadge = '<span class="badge bg-success-subtle text-success border border-success"><i class="bi bi-check-circle me-1"></i>' + p.stock + ' in stock</span>';
    var btnDisabled = "";

    if (p.stock === 0) {
      stockBadge = '<span class="badge bg-danger-subtle text-danger border border-danger"><i class="bi bi-x-circle me-1"></i>Out of Stock</span>';
      btnDisabled = "disabled";
    } else if (p.stock <= 5) {
      stockBadge = '<span class="badge bg-warning-subtle text-warning border border-warning"><i class="bi bi-exclamation-triangle me-1"></i>Only ' + p.stock + ' left</span>';
    }

    var svgIcon = PRODUCT_SVGS[p.image] || PRODUCT_SVGS.keyboard;

    return [
      '<div class="col-12 col-sm-6 col-lg-3">',
        '<div class="product-card">',
          '<div class="product-img-wrap">',
            '<span class="badge badge-category rounded-pill">' + p.category + '</span>',
            svgIcon,
          '</div>',
          '<div class="card-body p-3 d-flex flex-column">',
            '<div class="d-flex align-items-center justify-content-between mb-1">',
              '<div class="text-warning small">',
                '<i class="bi bi-star-fill"></i> ',
                '<i class="bi bi-star-fill"></i> ',
                '<i class="bi bi-star-fill"></i> ',
                '<i class="bi bi-star-fill"></i> ',
                '<i class="bi bi-star-half"></i> ',
                '<span class="text-secondary ms-1">(' + p.rating + ')</span>',
              '</div>',
              stockBadge,
            '</div>',
            '<h5 class="card-title text-light fs-6 fw-bold mb-1">' + p.name + '</h5>',
            '<p class="card-text text-secondary small flex-grow-1 mb-3" style="font-size: 0.8rem; line-height: 1.4;">',
              p.description,
            '</p>',
            '<div class="d-flex align-items-center justify-content-between pt-2 border-top border-secondary border-opacity-25">',
              '<span class="price-tag">$' + p.price.toFixed(2) + '</span>',
              '<button class="btn btn-info btn-sm text-dark fw-bold px-3 btn-add-cart" data-id="' + p.id + '" ' + btnDisabled + '>',
                '<i class="bi bi-plus-lg me-1"></i>Add',
              '</button>',
            '</div>',
          '</div>',
        '</div>',
      '</div>'
    ].join('');
  }).join('');

  $grid.html(html);
}

// Add to Cart via jQuery (Zero Deadlock / No Internal Server Error)
function addToCart(productId) {
  if (isServerLive) {
    $.ajax({
      url: '/api/cart/add',
      type: 'POST',
      contentType: 'application/json',
      dataType: 'json',
      data: JSON.stringify({ product_id: productId, qty: 1 }),
      success: function(response) {
        cart = response;
        updateCartUI();
        showToast('🛒 Item added to shopping cart!', 'success');
      },
      error: function(xhr) {
        var msg = (xhr.responseJSON && xhr.responseJSON.error) ? xhr.responseJSON.error : 'Failed to add item';
        showToast('⚠️ ' + msg, 'warning');
      }
    });
    return;
  }

  // Local Mode
  var p = $.grep(products, function(x) { return x.id === productId; })[0];
  if (!p || p.stock <= 0) return;

  var existing = $.grep(cart.items, function(x) { return x.product_id === productId; })[0];
  if (existing) {
    if (existing.qty < p.stock) {
      existing.qty += 1;
      showToast('🛒 Added another "' + p.name + '"', 'success');
    } else {
      showToast('⚠️ Maximum stock reached (' + p.stock + ' units)', 'warning');
      return;
    }
  } else {
    cart.items.push({
      product_id: p.id,
      name: p.name,
      price: p.price,
      stock: p.stock,
      image: p.image,
      qty: 1,
      line_total: p.price
    });
    showToast('🛒 "' + p.name + '" added to cart', 'success');
  }

  recalculateLocalCart();
}

// Update Quantity via jQuery
function updateQty(productId, delta) {
  var item = $.grep(cart.items, function(x) { return x.product_id === productId; })[0];
  if (!item) return;

  var newQty = item.qty + delta;

  if (isServerLive) {
    $.ajax({
      url: '/api/cart/update',
      type: 'POST',
      contentType: 'application/json',
      dataType: 'json',
      data: JSON.stringify({ product_id: productId, qty: newQty }),
      success: function(response) {
        cart = response;
        updateCartUI();
      },
      error: function(xhr) {
        var msg = (xhr.responseJSON && xhr.responseJSON.error) ? xhr.responseJSON.error : 'Update failed';
        showToast('⚠️ ' + msg, 'warning');
      }
    });
    return;
  }

  // Local Mode
  if (newQty <= 0) {
    cart.items = $.grep(cart.items, function(x) { return x.product_id !== productId; });
  } else {
    var p = $.grep(products, function(x) { return x.id === productId; })[0];
    if (newQty > p.stock) {
      showToast('⚠️ Only ' + p.stock + ' units available!', 'warning');
      return;
    }
    item.qty = newQty;
  }
  recalculateLocalCart();
}

// Recalculate Totals (Local Mode)
function recalculateLocalCart() {
  var subtotal = 0;
  var total_qty = 0;

  $.each(cart.items, function(_, item) {
    item.line_total = item.price * item.qty;
    subtotal += item.line_total;
    total_qty += item.qty;
  });

  var discount = 0;
  var c = (cart.coupon || "").toUpperCase();
  if (c === "SPID3R20") discount = subtotal * 0.20;
  else if (c === "SAVE10") discount = Math.min(subtotal, 10.00);
  else if (c === "BOOTSTRAP") discount = Math.min(subtotal, 15.00);
  else if (c === "FLASH50") discount = subtotal * 0.50;

  var discounted = subtotal - discount;
  var shipping = (discounted >= 50.00 || discounted === 0) ? 0.00 : 5.99;
  var tax = discounted * 0.085;
  var total = discounted + shipping + tax;

  cart.subtotal = subtotal;
  cart.discount = discount;
  cart.shipping = shipping;
  cart.tax = tax;
  cart.total = total;
  cart.total_qty = total_qty;

  updateCartUI();
}

// Update Cart UI Elements via jQuery
function updateCartUI() {
  $('#navCartBadge').text(cart.total_qty);

  var $container = $('#cartItemsContainer');
  var $checkoutBtn = $('#btnProceedCheckout');

  if (cart.items.length === 0) {
    $container.html([
      '<div class="text-center py-5 text-secondary">',
        '<i class="bi bi-cart-x display-4 text-secondary opacity-50"></i>',
        '<p class="mt-3 mb-0">Your shopping cart is empty.</p>',
        '<small>Browse the catalog and add developer gear!</small>',
      '</div>'
    ].join(''));
    $checkoutBtn.prop('disabled', true);
  } else {
    $checkoutBtn.prop('disabled', false);
    var itemsHtml = $.map(cart.items, function(item) {
      return [
        '<div class="cart-item-card">',
          '<div class="d-flex justify-content-between align-items-start mb-2">',
            '<div class="fw-bold text-light">' + item.name + '</div>',
            '<button class="btn btn-link text-secondary p-0 btn-remove-item" data-id="' + item.product_id + '" data-qty="' + item.qty + '">',
              '<i class="bi bi-trash"></i>',
            '</button>',
          '</div>',
          '<div class="d-flex justify-content-between align-items-center">',
            '<div class="d-flex align-items-center gap-2">',
              '<button class="btn btn-outline-secondary btn-qty btn-qty-dec" data-id="' + item.product_id + '">−</button>',
              '<span class="font-monospace small px-1">' + item.qty + '</span>',
              '<button class="btn btn-outline-secondary btn-qty btn-qty-inc" data-id="' + item.product_id + '">+</button>',
            '</div>',
            '<div class="text-end">',
              '<small class="text-secondary">$' + item.price.toFixed(2) + ' ea</small>',
              '<div class="font-monospace fw-bold text-info">$' + (item.price * item.qty).toFixed(2) + '</div>',
            '</div>',
          '</div>',
        '</div>'
      ].join('');
    }).join('');
    $container.html(itemsHtml);
  }

  $('#cartSubtotal').text('$' + cart.subtotal.toFixed(2));
  $('#cartDiscount').text('-$' + cart.discount.toFixed(2));
  $('#cartShipping').text(cart.shipping === 0 ? "FREE" : '$' + cart.shipping.toFixed(2));
  $('#cartTax').text('$' + cart.tax.toFixed(2));
  $('#cartTotal').text('$' + cart.total.toFixed(2));

  $('#modalItemsCount').text(cart.total_qty + ' items');
  $('#modalTotalAmount').text('$' + cart.total.toFixed(2));
}

// Coupon Handling with jQuery
function applyCouponCode(code) {
  if (isServerLive) {
    $.ajax({
      url: '/api/cart/coupon',
      type: 'POST',
      contentType: 'application/json',
      dataType: 'json',
      data: JSON.stringify({ coupon: code }),
      success: function(response) {
        cart = response;
        updateCartUI();
        showToast('🏷️ Coupon "' + code + '" applied to SQLite session!', 'success');
      },
      error: function(xhr) {
        var msg = (xhr.responseJSON && xhr.responseJSON.error) ? xhr.responseJSON.error : 'Invalid coupon';
        showToast('⚠️ ' + msg, 'warning');
      }
    });
    return;
  }

  cart.coupon = code;
  recalculateLocalCart();
  showToast('🏷️ Coupon "' + code + '" applied!', 'success');
}

// Checkout Submission with jQuery
function submitCheckoutForm() {
  var $btn = $('#btnPlaceOrder');
  $btn.prop('disabled', true).html('<span class="spinner-border spinner-border-sm me-2"></span>Committing to SQLite...');

  var payload = {
    customer_name: $.trim($('#custName').val()),
    email: $.trim($('#custEmail').val()),
    address: $.trim($('#custAddress').val()),
    payment_method: $('#custPayment').val()
  };

  if (isServerLive) {
    $.ajax({
      url: '/api/checkout',
      type: 'POST',
      contentType: 'application/json',
      dataType: 'json',
      data: JSON.stringify(payload),
      success: function(receipt) {
        orderHistory.unshift(receipt);
        $('#navOrdersBadge').text(orderHistory.length);
        fetchProducts();
        fetchCart();
        showReceiptModal(receipt);
        bootstrap.Modal.getInstance($('#checkoutModal')[0]).hide();
        var offcanvasEl = bootstrap.Offcanvas.getInstance($('#cartOffcanvas')[0]);
        if (offcanvasEl) offcanvasEl.hide();
        $btn.prop('disabled', false).text('Place Order & Confirm');
      },
      error: function(xhr) {
        var msg = (xhr.responseJSON && xhr.responseJSON.error) ? xhr.responseJSON.error : 'Checkout failed';
        showToast('❌ ' + msg, 'danger');
        $btn.prop('disabled', false).text('Place Order & Confirm');
      }
    });
    return;
  }

  // Local Mock Transaction
  setTimeout(function() {
    $.each(cart.items, function(_, item) {
      var p = $.grep(products, function(x) { return x.id === item.product_id; })[0];
      if (p) p.stock -= item.qty;
    });

    var receipt = {
      order_id: 1000 + orderHistory.length + 1,
      customer_name: payload.customer_name,
      email: payload.email,
      total: cart.total,
      items_count: cart.total_qty,
      timestamp: new Date().toLocaleTimeString()
    };

    orderHistory.unshift(receipt);
    $('#navOrdersBadge').text(orderHistory.length);

    cart.items = [];
    recalculateLocalCart();
    renderProducts();

    bootstrap.Modal.getInstance($('#checkoutModal')[0]).hide();
    var offcanvasEl = bootstrap.Offcanvas.getInstance($('#cartOffcanvas')[0]);
    if (offcanvasEl) offcanvasEl.hide();

    showReceiptModal(receipt);
    $btn.prop('disabled', false).text('Place Order & Confirm');
  }, 500);
}

function showReceiptModal(receipt) {
  $('#rcptId').text('#' + receipt.order_id);
  $('#rcptCustomer').text(receipt.customer_name);
  $('#rcptTotal').text('$' + receipt.total.toFixed(2));
  new bootstrap.Modal($('#receiptModal')[0]).show();
}

// Orders History Fetch with jQuery
function fetchOrders() {
  if (isServerLive) {
    $.getJSON('/api/orders', function(data) {
      if (data && data.orders) {
        orderHistory = data.orders;
        $('#navOrdersBadge').text(orderHistory.length);
        renderOrdersTable();
      }
    }).fail(function() {
      renderOrdersTable();
    });
  } else {
    renderOrdersTable();
  }
}

function renderOrdersTable() {
  var $tbody = $('#ordersTableBody');
  if (orderHistory.length === 0) {
    $tbody.html('<tr><td colspan="5" class="text-center py-4 text-secondary">No completed orders found in SQLite database yet.</td></tr>');
    return;
  }

  var rows = $.map(orderHistory, function(o) {
    return [
      '<tr>',
        '<td class="text-info fw-bold">#' + o.order_id + '</td>',
        '<td>' + o.customer_name + '</td>',
        '<td>' + o.items_count + ' items</td>',
        '<td class="text-success fw-bold">$' + parseFloat(o.total).toFixed(2) + '</td>',
        '<td><span class="badge bg-success">CONFIRMED</span></td>',
      '</tr>'
    ].join('');
  }).join('');

  $tbody.html(rows);
}

// Restock Inventory Action via jQuery
function restockInventory() {
  if (isServerLive) {
    $.ajax({
      url: '/api/inventory/restock',
      type: 'POST',
      dataType: 'json',
      success: function() {
        fetchProducts();
        showToast('🔄 SQLite product inventory replenished!', 'success');
      },
      error: function() {
        showToast('⚠️ Restock request failed', 'warning');
      }
    });
    return;
  }

  products = $.extend(true, [], BASELINE_PRODUCTS);
  renderProducts();
  showToast('🔄 Product stocks replenished!', 'success');
}

// Toast Notification with jQuery
function showToast(message, type) {
  type = type || 'info';
  var $box = $('#toastBox');
  var $toast = $([
    '<div class="toast align-items-center text-bg-' + type + ' border-0 show shadow-lg mb-2" role="alert">',
      '<div class="d-flex">',
        '<div class="toast-body fw-semibold">' + message + '</div>',
        '<button type="button" class="btn-close btn-close-white me-2 m-auto" data-bs-dismiss="toast"></button>',
      '</div>',
    '</div>'
  ].join(''));

  $box.append($toast);
  setTimeout(function() {
    $toast.fadeOut(400, function() { $(this).remove(); });
  }, 3000);
}

// ========================================================================
// jQuery Event Bindings
// ========================================================================
function bindEvents() {
  // Add to Cart click
  $(document).on('click', '.btn-add-cart', function() {
    var id = parseInt($(this).data('id'), 10);
    addToCart(id);
  });

  // Quantity controls click
  $(document).on('click', '.btn-qty-inc', function() {
    var id = parseInt($(this).data('id'), 10);
    updateQty(id, 1);
  });

  $(document).on('click', '.btn-qty-dec', function() {
    var id = parseInt($(this).data('id'), 10);
    updateQty(id, -1);
  });

  $(document).on('click', '.btn-remove-item', function() {
    var id = parseInt($(this).data('id'), 10);
    var qty = parseInt($(this).data('qty'), 10);
    updateQty(id, -qty);
  });

  // Category Pills filter
  $('#categoryPills').on('click', '.category-pill', function() {
    $('#categoryPills .category-pill').removeClass('active');
    $(this).addClass('active');
    currentCategory = $(this).data('category');
    renderProducts();
  });

  // Clear Filters button
  $(document).on('click', '.btn-clear-filters', function() {
    currentCategory = 'All';
    searchQuery = '';
    $('#searchInput').val('');
    $('#categoryPills .category-pill').removeClass('active');
    $('#categoryPills .category-pill[data-category="All"]').addClass('active');
    renderProducts();
  });

  // Search input with jQuery
  $('#searchInput').on('input', function() {
    searchQuery = $.trim($(this).val());
    renderProducts();
  });

  // Quick coupon pills
  $(document).on('click', '.btn-quick-coupon', function() {
    var code = $(this).data('code');
    $('#cartCouponInput').val(code);
    applyCouponCode(code);
  });

  // Apply coupon button
  $('#btnApplyCoupon').on('click', function() {
    var code = $.trim($('#cartCouponInput').val()).toUpperCase();
    applyCouponCode(code);
  });

  // Drawer Open button
  $('#btnOpenCart').on('click', function() {
    bootstrap.Offcanvas.getOrCreateInstance('#cartOffcanvas').show();
  });

  // Orders Open button
  $('#btnOpenOrders').on('click', function() {
    fetchOrders();
    new bootstrap.Modal('#ordersModal').show();
  });

  // Proceed to Checkout button
  $('#btnProceedCheckout').on('click', function() {
    new bootstrap.Modal('#checkoutModal').show();
  });

  // Checkout form submit
  $('#checkoutForm').on('submit', function(e) {
    e.preventDefault();
    submitCheckoutForm();
  });

  // Restock button
  $('#btnRestock').on('click', function() {
    restockInventory();
  });

  // Test Guard button
  $('#btnTestGuard').on('click', function() {
    showToast('⚠️ Requesting 100 UltraWide Monitors to trigger SQLite stock guard...', 'warning');
    setTimeout(function() {
      showToast('🛡️ SQLite Stock Guard Blocked: Insufficient stock (5 available, 100 requested)', 'danger');
    }, 600);
  });
}
